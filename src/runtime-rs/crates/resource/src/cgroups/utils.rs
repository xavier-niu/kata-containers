// Copyright (c) 2019-2022 Alibaba Cloud
// Copyright (c) 2019-2022 Ant Group
//
// SPDX-License-Identifier: Apache-2.0
//

use anyhow::{anyhow, Context, Result};

// When the Kata overhead threads (I/O, VMM, etc) are not
// placed in the sandbox resource controller (A cgroup on Linux),
// they are moved to a specific, unconstrained resource controller.
// On Linux, assuming the cgroup mount point is at /sys/fs/cgroup/,
// on a cgroup v1 system, the Kata overhead memory cgroup will be at
// /sys/fs/cgroup/memory/kata_overhead/$CGPATH where $CGPATH is
// defined by the orchestrator.
pub(crate) fn gen_overhead_path(path: &str) -> String {
    format!("kata_overhead/{}", path.trim_start_matches('/'))
}

/// Get the thread group ID (TGID) from `/proc/{pid}/status`.
pub(crate) fn get_tgid_from_pid(pid: i32) -> Result<i32> {
    let status = std::fs::read_to_string(format!("/proc/{}/status", pid))
        .map_err(|e| anyhow!("failed to read /proc/{}/status: {}", pid, e))?;
    parse_tgid_from_proc_status(&status)
        .with_context(|| anyhow!("failed to parse tgid from /proc/{}/status", pid))
}

fn parse_tgid_from_proc_status(lines: &str) -> Result<i32> {
    lines
        .lines()
        .find_map(|line| {
            if line.starts_with("Tgid") {
                let part = line.split(":").nth(1)?;
                part.trim().parse::<i32>().ok()
            } else {
                None
            }
        })
        .ok_or(anyhow!("tgid not found"))
}

#[cfg(test)]
mod tests {
    use crate::cgroups::utils::*;

    #[test]
    fn test_parse_tgid_from_proc_status() {
        let status = r#"Name:	systemd
Umask:	0000
State:	S (sleeping)
Tgid:	1
Ngid:	0
Pid:	1
PPid:	0
TracerPid:	0
Uid:	0	0	0	0
Gid:	0	0	0	0
FDSize:	256
Groups:
NStgid:	1
NSpid:	1
NSpgid:	1
NSsid:	1
Kthread:	0
VmPeak:	   23132 kB
VmSize:	   23104 kB
VmLck:	       0 kB
VmPin:	       0 kB
VmHWM:	   13660 kB
VmRSS:	   13660 kB
RssAnon:	    4160 kB
RssFile:	    9500 kB
RssShmem:	       0 kB
VmData:	    4080 kB
VmStk:	     132 kB
VmExe:	      44 kB
VmLib:	   12188 kB
VmPTE:	      92 kB
VmSwap:	       0 kB
HugetlbPages:	       0 kB
CoreDumping:	0
THP_enabled:	1
untag_mask:	0xffffffffffffffff
Threads:	1
SigQ:	2/63648
SigPnd:	0000000000000000
ShdPnd:	0000000000000000
SigBlk:	7fefc1fe28014a03
SigIgn:	0000000000001000
SigCgt:	00000000000004ec
CapInh:	0000000000000000
CapPrm:	000001ffffffffff
CapEff:	000001ffffffffff
CapBnd:	000001ffffffffff
CapAmb:	0000000000000000
NoNewPrivs:	0
Seccomp:	0
Seccomp_filters:	0
Speculation_Store_Bypass:	vulnerable
SpeculationIndirectBranch:	always enabled
Cpus_allowed:	fffff
Cpus_allowed_list:	0-19
Mems_allowed:	00000000,00000000,00000000,00000000,00000000,00000000,00000000,00000000,00000000,00000000,00000000,00000000,00000000,00000000,00000000,00000000,00000000,00000000,00000000,00000000,00000000,00000000,00000000,00000000,00000000,00000000,00000000,00000000,00000000,00000000,00000000,00000001
Mems_allowed_list:	0
voluntary_ctxt_switches:	2146
nonvoluntary_ctxt_switches:	1341
x86_Thread_features:
x86_Thread_features_locked:
"#;

        let tgid = parse_tgid_from_proc_status(status).unwrap();
        assert_eq!(tgid, 1);
    }
}
