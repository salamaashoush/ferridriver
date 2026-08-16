// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0
use std::ffi::CStr;

use libc::{getpriority, setpriority, PRIO_PROCESS};
use once_cell::sync::Lazy;
use rquickjs::{
    prelude::{Opt, Rest},
    Ctx, Exception, IntoJs, Null, Object, Result, Value,
};

use crate::os::get_home_dir;

static OS_INFO: Lazy<(String, String, String)> = Lazy::new(uname);
pub static EOL: &str = "\n";
pub static DEV_NULL: &str = "/dev/null";

pub fn get_priority(who: Opt<u32>) -> i32 {
    let who = who.0.unwrap_or(0);

    unsafe { getpriority(PRIO_PROCESS, who) }
}

pub fn set_priority(ctx: Ctx<'_>, args: Rest<Value>) -> Result<()> {
    let mut args_iter = args.0.into_iter().rev();
    let prio: i32 = args_iter
        .next()
        .and_then(|v| v.as_number())
        .ok_or_else(|| {
            Exception::throw_type(&ctx, "The `priority` argument must be of type number.")
        })? as i32;
    let who: u32 = args_iter.next().and_then(|v| v.as_number()).unwrap_or(0f64) as u32;

    if !(-20..=19).contains(&prio) {
        return Err(Exception::throw_range(
            &ctx,
            "The value of `priority` is out of range. It must be >= -20 && <= 19.",
        ));
    }

    unsafe {
        setpriority(PRIO_PROCESS, who, prio);
    }
    Ok(())
}

pub fn get_type() -> &'static str {
    &OS_INFO.0
}

pub fn get_user_info<'js>(ctx: Ctx<'js>, _options: Opt<Value>) -> Result<Object<'js>> {
    let obj = Object::new(ctx.clone())?;

    // SAFETY: `getuid`/`getgid` take no arguments and cannot fail.
    let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
    obj.set("uid", uid)?;
    obj.set("gid", gid)?;

    let (username, shell) = match passwd_entry(uid) {
        Some((name, shell)) => (name.into_js(&ctx)?, shell.into_js(&ctx)?),
        None => (Null.into_js(&ctx)?, Null.into_js(&ctx)?),
    };
    obj.set("username", username)?;
    obj.set("homedir", get_home_dir(ctx.clone()))?;
    obj.set("shell", shell)?;
    Ok(obj)
}

/// The login name and shell of `uid`, from the password database.
///
/// Local delta: upstream reads these through the `users` crate, which has
/// been unmaintained since 2021. `getpwuid_r` is the call that crate makes.
fn passwd_entry(uid: libc::uid_t) -> Option<(String, String)> {
    // `sysconf(_SC_GETPW_R_SIZE_MAX)` is a hint, not a bound: it can report
    // -1, and an entry can exceed it, in which case `getpwuid_r` answers
    // ERANGE and the buffer has to grow.
    // SAFETY: `sysconf` takes an integer name and cannot fail destructively.
    let hint = unsafe { libc::sysconf(libc::_SC_GETPW_R_SIZE_MAX) };
    let mut size = if hint > 0 { hint as usize } else { 1024 };

    loop {
        let mut buf = vec![0_i8; size];
        let mut passwd: libc::passwd = unsafe { std::mem::zeroed() };
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        // SAFETY: `passwd` and `result` are valid out-pointers and `buf` is
        // `size` bytes long, which is what is passed.
        let rc = unsafe {
            libc::getpwuid_r(
                uid,
                &raw mut passwd,
                buf.as_mut_ptr(),
                size,
                &raw mut result,
            )
        };
        if rc == libc::ERANGE && size < 1 << 20 {
            size *= 2;
            continue;
        }
        if rc != 0 || result.is_null() {
            return None;
        }
        // SAFETY: on success both pointers are NUL-terminated strings owned
        // by `buf`, which outlives the copies made here.
        let (name, shell) = unsafe {
            (
                CStr::from_ptr(passwd.pw_name).to_string_lossy().into_owned(),
                CStr::from_ptr(passwd.pw_shell).to_string_lossy().into_owned(),
            )
        };
        return Some((name, shell));
    }
}

pub fn get_release() -> &'static str {
    &OS_INFO.1
}

pub fn get_version() -> &'static str {
    &OS_INFO.2
}

fn uname() -> (String, String, String) {
    let mut info = std::mem::MaybeUninit::uninit();
    // SAFETY: `info` is a valid pointer to a `libc::utsname` struct.
    let res = unsafe { libc::uname(info.as_mut_ptr()) };
    if res != 0 {
        return (String::new(), String::new(), String::new());
    }
    // SAFETY: `uname` returns 0 on success and info is initialized.
    let info = unsafe { info.assume_init() };
    (
        // SAFETY: `info.sysname` is a valid NUL-terminated pointer.
        unsafe {
            CStr::from_ptr(info.sysname.as_ptr())
                .to_string_lossy()
                .into_owned()
        },
        // SAFETY: `info.release` is a valid NUL-terminated pointer.
        unsafe {
            CStr::from_ptr(info.release.as_ptr())
                .to_string_lossy()
                .into_owned()
        },
        // SAFETY: `info.version` is a valid NUL-terminated pointer.
        unsafe {
            CStr::from_ptr(info.version.as_ptr())
                .to_string_lossy()
                .into_owned()
        },
    )
}
