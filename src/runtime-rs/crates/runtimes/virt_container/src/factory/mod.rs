// Copyright 2025 Kata Contributors
//
// SPDX-License-Identifier: Apache-2.0
//

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use common::RuntimeHandler;
use hypervisor::HYPERVISOR_NAME_CH;
use kata_sys_util::mount::umount_all;
use kata_types::config::TomlConfig;
use serde::{Deserialize, Serialize};
use slog::{error, info, warn};

use crate::factory::{
    template::Template,
    vm::{TemplateVm, VmConfig},
};

pub mod template;
pub mod vm;

/// Returns the path to the hypervisor's device-state artifact in the template directory.
pub(crate) fn template_device_state_path(hypervisor_name: &str, template_path: &Path) -> PathBuf {
    let state_file = match hypervisor_name {
        HYPERVISOR_NAME_CH => "state.json",
        _ => "state",
    };

    template_path.join(state_file)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FactoryConfig {
    /// Path to the directory where VM templates are stored.
    #[serde(default)]
    pub template_path: String,

    /// Full configuration of the virtual machine to be used.
    #[serde(default)]
    pub vm_config: VmConfig,

    /// Whether VM template feature is enabled.
    #[serde(default)]
    pub template: bool,
}

impl FactoryConfig {
    pub fn new(toml_config: &TomlConfig) -> Self {
        Self {
            template: toml_config.get_factory().enable_template,
            template_path: toml_config.get_factory().template_path,
            vm_config: VmConfig::new(toml_config),
        }
    }
}

/// Load and validate factory configuration
fn load_and_validate_factory_config(
    config_path: Option<&Path>,
) -> Result<(TomlConfig, FactoryConfig)> {
    crate::VirtContainer::init().context("initialize runtime handler")?;

    let (toml_config, _) = match config_path {
        Some(config_path) => TomlConfig::load_from_file(config_path),
        None => TomlConfig::load_from_default(),
    }
    .context("load toml config")?;

    let factory_config = FactoryConfig::new(&toml_config);

    if !factory_config.template {
        return Err(anyhow!("vm factory is not enabled"));
    }

    Ok((toml_config, factory_config))
}

pub async fn init_factory_command(config_path: Option<&Path>) -> Result<()> {
    let (toml_config, mut factory_config) = load_and_validate_factory_config(config_path)?;

    new_factory(&mut factory_config, toml_config, false)
        .await
        .context("new factory")?;

    info!(sl!(), "create vm factory successfully");

    Ok(())
}

pub async fn destroy_factory_command(config_path: Option<&Path>) -> Result<()> {
    let (toml_config, mut factory_config) = load_and_validate_factory_config(config_path)?;

    new_factory(&mut factory_config, toml_config, true)
        .await
        .context("new factory")?;

    close_factory(&mut factory_config).context(" close VM factory")?;

    info!(sl!(), "vm factory destroyed");
    Ok(())
}

pub async fn status_factory_command(config_path: Option<&Path>) -> Result<()> {
    let (toml_config, mut factory_config) = load_and_validate_factory_config(config_path)?;

    if new_factory(&mut factory_config, toml_config, true)
        .await
        .is_ok()
    {
        info!(sl!(), "vm factory is on");
    } else {
        info!(sl!(), "vm factory is off");
    }

    Ok(())
}

/// Benchmark the common VM-template restore boundary used by all supported
/// hypervisors: create the restored VM and connect to its guest agent. VM
/// teardown deliberately happens after the timer stops.
pub async fn benchmark_factory_command(
    config_path: Option<&Path>,
    iterations: usize,
    warmups: usize,
    cold_start: bool,
) -> Result<()> {
    if iterations == 0 {
        return Err(anyhow!("iterations must be greater than zero"));
    }

    let (_, mut factory_config) = load_and_validate_factory_config(config_path)?;
    VmConfig::validate_hypervisor_config(&mut factory_config.vm_config.hypervisor_config)
        .context("validate hypervisor config")?;

    let benchmark_config = if cold_start {
        factory_config.vm_config.clone()
    } else {
        let template = Template::fetch(
            factory_config.vm_config.clone(),
            PathBuf::from(&factory_config.template_path),
        )
        .context("fetch VM template")?;
        template.prepare_vm_config(false)
    };
    let hypervisor_name = benchmark_config.hypervisor_name.clone();
    let mode = if cold_start { "cold" } else { "template" };

    info!(
        sl!(),
        "benchmarking VM boot through guest-agent readiness";
        "hypervisor" => hypervisor_name.clone(),
        "mode" => mode,
        "iterations" => iterations,
        "warmups" => warmups,
    );

    for warmup in 1..=warmups {
        let (mut trial_config, _) = load_and_validate_factory_config(config_path)?;
        if cold_start {
            trial_config
                .hypervisor
                .get_mut(&hypervisor_name)
                .context("get cold-start hypervisor config")?
                .factory
                .enable_template = false;
        }
        let vm = TemplateVm::new_vm(benchmark_config.clone(), trial_config)
            .await
            .with_context(|| format!("warmup {warmup} VM boot"))?;
        vm.teardown()
            .await
            .with_context(|| format!("warmup {warmup} teardown"))?;
    }

    let mut elapsed = Vec::with_capacity(iterations);
    for iteration in 1..=iterations {
        let (mut trial_config, _) = load_and_validate_factory_config(config_path)?;
        if cold_start {
            trial_config
                .hypervisor
                .get_mut(&hypervisor_name)
                .context("get cold-start hypervisor config")?
                .factory
                .enable_template = false;
        }
        let start = Instant::now();
        let vm = TemplateVm::new_vm(benchmark_config.clone(), trial_config)
            .await
            .with_context(|| format!("iteration {iteration} VM boot"))?;
        let trial_elapsed = start.elapsed();
        println!(
            "RESULT hypervisor={} mode={} iteration={} elapsed={:.6}s",
            hypervisor_name,
            mode,
            iteration,
            trial_elapsed.as_secs_f64()
        );
        elapsed.push(trial_elapsed);

        vm.teardown()
            .await
            .with_context(|| format!("iteration {iteration} teardown"))?;
    }

    let total: Duration = elapsed.iter().copied().sum();
    let mean = total.as_secs_f64() / iterations as f64;
    elapsed.sort_unstable();
    let median = if iterations.is_multiple_of(2) {
        (elapsed[iterations / 2 - 1].as_secs_f64() + elapsed[iterations / 2].as_secs_f64()) / 2.0
    } else {
        elapsed[iterations / 2].as_secs_f64()
    };
    let p95_index = (iterations * 95).div_ceil(100).saturating_sub(1);
    let p95 = elapsed[p95_index].as_secs_f64();

    println!(
        "SUMMARY hypervisor={} mode={} iterations={} mean={:.6}s median={:.6}s p95={:.6}s",
        hypervisor_name, mode, iterations, mean, median, p95
    );

    Ok(())
}

pub async fn new_factory(
    config: &mut FactoryConfig,
    toml_config: TomlConfig,
    fetch_only: bool,
) -> Result<()> {
    if !config.template {
        anyhow::bail!("template must be enabled");
    } else {
        VmConfig::validate_hypervisor_config(&mut config.vm_config.hypervisor_config)
            .context("validate hypervisor config")?;

        let path: PathBuf = config.template_path.clone().into();
        if fetch_only {
            Template::fetch(config.vm_config.clone(), path).context("fetch VM template")?;
        } else {
            Template::create(config.vm_config.clone(), toml_config, path)
                .await
                .context("initialize VM template factory")?;
        }
    }

    Ok(())
}

pub fn close_factory(config: &mut FactoryConfig) -> Result<()> {
    let state_path = Path::new(&config.template_path);

    // Check if the path exists
    if !state_path.exists() {
        warn!(
            sl!(),
            "Template path {:?} does not exist, skipping unmount", state_path
        );
        return Ok(());
    }

    // Use umount_all to unmount all filesystems at the mountpoint
    // First try normal umount (lazy_umount = false)
    if let Err(e) = umount_all(state_path, false) {
        error!(sl!(), "Normal umount failed for {:?}: {}", state_path, e);

        // If normal umount fails, try lazy umount (with MNT_DETACH flag)
        umount_all(state_path, true)
            .with_context(|| format!("Failed to lazy unmount {}", state_path.display()))?;

        info!(sl!(), "Lazy umount succeeded for {:?}", state_path);
    } else {
        info!(sl!(), "Normal umount succeeded for {:?}", state_path);
    }

    // Remove the directory after successful unmount
    fs::remove_dir_all(state_path)
        .with_context(|| format!("failed to remove {}", state_path.display()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kata_types::config::QemuConfig;
    use uuid::Uuid;

    #[test]
    fn test_custom_factory_config_is_adjusted() {
        QemuConfig::new().register();

        let config_path =
            std::env::temp_dir().join(format!("kata-factory-config-{}.toml", Uuid::new_v4()));
        fs::write(
            &config_path,
            r#"
[hypervisor.qemu]
path = "/dev/null"
ctlpath = "/dev/null"
kernel = "/dev/null"
image = "/dev/null"
entropy_source = "/dev/urandom"
shared_fs = "none"

[hypervisor.qemu.factory]
enable_template = true
template_path = "/tmp/kata-vm-template"

[runtime]
hypervisor_name = "qemu"
agent_name = "kata"
"#,
        )
        .unwrap();

        let result = load_and_validate_factory_config(Some(&config_path));
        fs::remove_file(&config_path).unwrap();

        let (toml_config, factory_config) = result.unwrap();
        assert!(toml_config.hypervisor["qemu"].shared_fs.shared_fs.is_none());
        assert!(factory_config
            .vm_config
            .hypervisor_config
            .shared_fs
            .shared_fs
            .is_none());
    }
}
