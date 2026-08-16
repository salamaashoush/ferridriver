// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0
use std::sync::{Arc, Mutex};

use once_cell::sync::Lazy;
use rquickjs::{Ctx, Object, Result};
use sysinfo::{CpuRefreshKind, MemoryRefreshKind, RefreshKind, System};

static SYSTEM: Lazy<Arc<Mutex<System>>> = Lazy::new(|| {
    Arc::new(Mutex::new(System::new_with_specifics(
        RefreshKind::nothing()
            .with_cpu(CpuRefreshKind::nothing().with_cpu_usage().with_frequency())
            .with_memory(MemoryRefreshKind::nothing().with_ram()),
    )))
});

/// Cumulative milliseconds one logical CPU spent in each mode, in Node's
/// `os.cpus()[].times` shape.
///
/// Local delta: upstream sets every field to 0 with the comment "cannot be
/// obtained at this time". sysinfo does not expose them, but the kernel
/// does — through `/proc/stat` on Linux and `host_processor_info` on macOS,
/// which is where libuv reads them for Node.
#[derive(Clone, Copy, Default)]
pub struct CpuTimes {
    pub user: u64,
    pub nice: u64,
    pub sys: u64,
    pub idle: u64,
    pub irq: u64,
}

/// Milliseconds per clock tick, for counters the kernel reports in ticks.
#[cfg(unix)]
fn tick_ms() -> f64 {
    // SAFETY: `sysconf` takes an integer name and has no side effects.
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if hz > 0 {
        1000.0 / hz as f64
    } else {
        // The historical default every Unix uses when `_SC_CLK_TCK` is
        // unavailable.
        1000.0 / 100.0
    }
}

#[cfg(target_os = "linux")]
fn cpu_times() -> Vec<CpuTimes> {
    let Ok(stat) = std::fs::read_to_string("/proc/stat") else {
        return Vec::new();
    };
    let ms = tick_ms();
    let field = |parts: &[&str], i: usize| -> u64 {
        parts
            .get(i)
            .and_then(|v| v.parse::<u64>().ok())
            .map_or(0, |ticks| (ticks as f64 * ms) as u64)
    };
    stat.lines()
        // `cpu` with no index is the aggregate line; the per-CPU lines are
        // `cpu0`, `cpu1`, ...
        .filter(|line| {
            line.strip_prefix("cpu")
                .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
        })
        .map(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            CpuTimes {
                user: field(&parts, 1),
                nice: field(&parts, 2),
                sys: field(&parts, 3),
                idle: field(&parts, 4),
                irq: field(&parts, 6),
            }
        })
        .collect()
}

// `libc` deprecates its mach bindings in favour of the `mach2` crate. This
// crate does not take a dependency for two symbols; the bindings are stable
// kernel ABI and are not going anywhere.
#[cfg(target_os = "macos")]
#[allow(deprecated)]
fn cpu_times() -> Vec<CpuTimes> {
    let mut count: libc::natural_t = 0;
    let mut info: libc::processor_info_array_t = std::ptr::null_mut();
    let mut info_count: libc::mach_msg_type_number_t = 0;

    // SAFETY: all three out-parameters are valid pointers; on success the
    // kernel allocates `info` in this task's address space and hands over
    // ownership, which is released below.
    let rc = unsafe {
        libc::host_processor_info(
            libc::mach_host_self(),
            libc::PROCESSOR_CPU_LOAD_INFO,
            &raw mut count,
            &raw mut info,
            &raw mut info_count,
        )
    };
    if rc != 0 || info.is_null() {
        return Vec::new();
    }

    let ms = tick_ms();
    let states = libc::CPU_STATE_MAX as usize;
    let mut out = Vec::with_capacity(count as usize);
    for cpu in 0..count as usize {
        let at = |state: libc::c_int| -> u64 {
            // SAFETY: the kernel returned `count * CPU_STATE_MAX` ticks and
            // the index stays inside that.
            let ticks = unsafe { *info.add(cpu * states + state as usize) } as f64;
            (ticks * ms) as u64
        };
        out.push(CpuTimes {
            user: at(libc::CPU_STATE_USER),
            nice: at(libc::CPU_STATE_NICE),
            sys: at(libc::CPU_STATE_SYSTEM),
            idle: at(libc::CPU_STATE_IDLE),
            // Darwin does not account interrupt time separately; libuv
            // reports 0 here too.
            irq: 0,
        });
    }

    // SAFETY: `info` is the buffer the kernel just allocated, of exactly
    // `info_count` natural_t entries.
    unsafe {
        libc::vm_deallocate(
            libc::mach_task_self_,
            info as libc::vm_address_t,
            info_count as usize * std::mem::size_of::<libc::natural_t>(),
        );
    }
    out
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn cpu_times() -> Vec<CpuTimes> {
    Vec::new()
}

pub fn get_cpus(ctx: Ctx<'_>) -> Result<Vec<Object<'_>>> {
    let mut vec: Vec<Object> = Vec::new();
    let system = SYSTEM.lock().unwrap();
    let times = cpu_times();

    for (index, cpu) in system.cpus().iter().enumerate() {
        let obj = Object::new(ctx.clone())?;
        obj.set("model", cpu.brand())?;
        obj.set("speed", cpu.frequency())?;

        let t = times.get(index).copied().unwrap_or_default();
        let times_obj = Object::new(ctx.clone())?;
        times_obj.set("user", t.user)?;
        times_obj.set("nice", t.nice)?;
        times_obj.set("sys", t.sys)?;
        times_obj.set("idle", t.idle)?;
        times_obj.set("irq", t.irq)?;
        obj.set("times", times_obj)?;

        vec.push(obj);
    }
    Ok(vec)
}

pub fn get_free_mem() -> u64 {
    let mut system = SYSTEM.lock().unwrap();

    system.refresh_memory_specifics(MemoryRefreshKind::nothing().with_ram());
    system.free_memory()
}

pub fn get_total_mem() -> u64 {
    let mut system = SYSTEM.lock().unwrap();

    system.refresh_memory_specifics(MemoryRefreshKind::nothing().with_ram());
    system.total_memory()
}
