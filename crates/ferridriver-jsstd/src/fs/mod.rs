// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0
mod access;
mod chmod;
mod file_handle;
mod mkdir;
mod open;
mod read_dir;
mod read_file;
mod rename;
mod rm;
mod stats;
mod symlink;
mod write_file;

use crate::utils::module::ModuleInfo;
use rquickjs::Value;
use rquickjs::{
    module::{Declarations, Exports, ModuleDef},
    prelude::{Async, Func},
};
use rquickjs::{Class, Ctx, Object, Result};

use self::access::{access, access_sync};
use self::chmod::{chmod, chmod_sync};
use self::file_handle::FileHandle;
use self::mkdir::{mkdir, mkdir_sync, mkdtemp, mkdtemp_sync};
use self::open::open;
use self::read_dir::{read_dir, read_dir_sync, Dirent};
use self::read_file::{read_file, read_file_sync};
use self::rename::{rename, rename_sync};
use self::rm::{rmdir, rmdir_sync, rmfile, rmfile_sync};
use self::stats::{lstat_fn, lstat_fn_sync, stat_fn, stat_fn_sync, Stats};
use self::symlink::{symlink, symlink_sync};
use self::write_file::{write_file, write_file_sync};

pub const CONSTANT_F_OK: u32 = 0;
pub const CONSTANT_R_OK: u32 = 4;
pub const CONSTANT_W_OK: u32 = 2;
pub const CONSTANT_X_OK: u32 = 1;

pub struct FsPromisesModule;

impl ModuleDef for FsPromisesModule {
    fn declare(declare: &Declarations) -> Result<()> {
        declare.declare("access")?;
        declare.declare("open")?;
        declare.declare("readFile")?;
        declare.declare("writeFile")?;
        declare.declare("rename")?;
        declare.declare("readdir")?;
        declare.declare("mkdir")?;
        declare.declare("mkdtemp")?;
        declare.declare("rm")?;
        declare.declare("rmdir")?;
        declare.declare("stat")?;
        declare.declare("lstat")?;
        declare.declare("constants")?;
        declare.declare("chmod")?;
        declare.declare("symlink")?;

        declare.declare("default")?;

        Ok(())
    }

    fn evaluate<'js>(ctx: &Ctx<'js>, exports: &Exports<'js>) -> Result<()> {
        // The VM's ONE `fs/promises` namespace, not a second copy: Node
        // answers the same object for `fs.promises` and for an import of
        // this specifier.
        export_object(exports, &fs_promises_object(ctx)?)
    }
}

impl From<FsPromisesModule> for ModuleInfo<FsPromisesModule> {
    fn from(val: FsPromisesModule) -> Self {
        ModuleInfo {
            name: "fs/promises",
            module: val,
        }
    }
}

pub struct FsModule;

impl ModuleDef for FsModule {
    fn declare(declare: &Declarations) -> Result<()> {
        declare.declare("promises")?;
        declare.declare("accessSync")?;
        declare.declare("mkdirSync")?;
        declare.declare("mkdtempSync")?;
        declare.declare("readdirSync")?;
        declare.declare("readFileSync")?;
        declare.declare("existsSync")?;
        declare.declare("rmdirSync")?;
        declare.declare("rmSync")?;
        declare.declare("statSync")?;
        declare.declare("lstatSync")?;
        declare.declare("writeFileSync")?;
        declare.declare("constants")?;
        declare.declare("chmodSync")?;
        declare.declare("renameSync")?;
        declare.declare("symlinkSync")?;

        declare.declare("default")?;

        Ok(())
    }

    fn evaluate<'js>(ctx: &Ctx<'js>, exports: &Exports<'js>) -> Result<()> {
        export_object(exports, &fs_object(ctx)?)
    }
}

/// LOCAL DELTA: `existsSync`.
///
/// Node has it and a large share of real code calls it; upstream llrt
/// ships neither it nor a callback API, so without this the only way to
/// ask whether a file is there is to catch a `stat` rejection.
fn exists_sync(path: String) -> bool {
    std::path::Path::new(&path).exists()
}

/// The `fs` namespace: every sync entry point, `promises`, `constants`.
///
/// Shared with the module definition so an `import fs from "node:fs"` and
/// a host that installs `fs` some other way cannot disagree about what
/// the surface is.
pub fn fill_fs<'js>(ctx: &Ctx<'js>, target: &Object<'js>) -> Result<()> {
    let promises = Object::new(ctx.clone())?;
    export_promises(ctx, &promises)?;
    export_constants(ctx, target)?;

    target.set("promises", promises)?;
    target.set("accessSync", Func::from(access_sync))?;
    target.set("mkdirSync", Func::from(mkdir_sync))?;
    target.set("mkdtempSync", Func::from(mkdtemp_sync))?;
    target.set("readdirSync", Func::from(read_dir_sync))?;
    target.set("readFileSync", Func::from(read_file_sync))?;
    target.set("existsSync", Func::from(exists_sync))?;
    target.set("rmdirSync", Func::from(rmdir_sync))?;
    target.set("rmSync", Func::from(rmfile_sync))?;
    target.set("statSync", Func::from(stat_fn_sync))?;
    target.set("lstatSync", Func::from(lstat_fn_sync))?;
    target.set("writeFileSync", Func::from(write_file_sync))?;
    target.set("chmodSync", Func::from(chmod_sync))?;
    target.set("renameSync", Func::from(rename_sync))?;
    target.set("symlinkSync", Func::from(symlink_sync))?;

    Ok(())
}

/// The VM's one `fs` namespace object.
///
/// Built once per context and remembered, because Node answers the same
/// object for every spelling: `require("fs") === require("node:fs")`, and
/// the global is that object too. Handing back a fresh one each time
/// would make those comparisons false and give a caller who patched a
/// method a copy nobody else sees.
pub fn fs_object<'js>(ctx: &Ctx<'js>) -> Result<Object<'js>> {
    cached(ctx, false)
}

/// The VM's one `fs/promises` namespace object.
pub fn fs_promises_object<'js>(ctx: &Ctx<'js>) -> Result<Object<'js>> {
    cached(ctx, true)
}

struct FsNamespaces {
    fs: rquickjs::Persistent<Object<'static>>,
    promises: rquickjs::Persistent<Object<'static>>,
}

// SAFETY: holds only `Persistent`s, which are lifetime-erased by
// construction.
#[allow(unsafe_code)]
unsafe impl rquickjs::JsLifetime<'_> for FsNamespaces {
    type Changed<'to> = FsNamespaces;
}

fn cached<'js>(ctx: &Ctx<'js>, promises: bool) -> Result<Object<'js>> {
    if let Some(ud) = ctx.userdata::<FsNamespaces>() {
        let held = if promises { ud.promises.clone() } else { ud.fs.clone() };
        if let Ok(obj) = held.restore(ctx) {
            return Ok(obj);
        }
    }
    define_classes(ctx)?;
    let fs = Object::new(ctx.clone())?;
    fill_fs(ctx, &fs)?;
    let promises_obj = Object::new(ctx.clone())?;
    export_promises(ctx, &promises_obj)?;
    // `fs.promises` and the `fs/promises` module are the same object in
    // Node, so the namespace built here is the one `fs` carries.
    fs.set("promises", promises_obj.clone())?;
    let answer = if promises { promises_obj.clone() } else { fs.clone() };
    let _ = ctx.store_userdata(FsNamespaces {
        fs: rquickjs::Persistent::save(ctx, fs),
        promises: rquickjs::Persistent::save(ctx, promises_obj),
    });
    Ok(answer)
}

/// `Dirent` / `FileHandle` / `Stats` are returned BY these functions, so
/// they have to be defined whichever way the surface was reached.
fn define_classes(ctx: &Ctx<'_>) -> Result<()> {
    let globals = ctx.globals();
    Class::<Dirent>::define(&globals)?;
    Class::<FileHandle>::define(&globals)?;
    Class::<Stats>::define(&globals)?;
    Ok(())
}

fn export_promises<'js>(ctx: &Ctx<'js>, exports: &Object<'js>) -> Result<()> {
    export_constants(ctx, exports)?;

    exports.set("access", Func::from(Async(access)))?;
    exports.set("open", Func::from(Async(open)))?;
    exports.set("readFile", Func::from(Async(read_file)))?;
    exports.set("writeFile", Func::from(Async(write_file)))?;
    exports.set("rename", Func::from(Async(rename)))?;
    exports.set("readdir", Func::from(Async(read_dir)))?;
    exports.set("mkdir", Func::from(Async(mkdir)))?;
    exports.set("mkdtemp", Func::from(Async(mkdtemp)))?;
    exports.set("rm", Func::from(Async(rmfile)))?;
    exports.set("rmdir", Func::from(Async(rmdir)))?;
    exports.set("stat", Func::from(Async(stat_fn)))?;
    exports.set("lstat", Func::from(Async(lstat_fn)))?;
    exports.set("chmod", Func::from(Async(chmod)))?;
    exports.set("symlink", Func::from(Async(symlink)))?;

    Ok(())
}

fn export_constants<'js>(ctx: &Ctx<'js>, exports: &Object<'js>) -> Result<()> {
    let constants = Object::new(ctx.clone())?;
    constants.set("F_OK", CONSTANT_F_OK)?;
    constants.set("R_OK", CONSTANT_R_OK)?;
    constants.set("W_OK", CONSTANT_W_OK)?;
    constants.set("X_OK", CONSTANT_X_OK)?;

    exports.set("constants", constants)?;

    Ok(())
}

impl From<FsModule> for ModuleInfo<FsModule> {
    fn from(val: FsModule) -> Self {
        ModuleInfo {
            name: "fs",
            module: val,
        }
    }
}

/// Install `fs` as a global.
///
/// Node has no global `fs`; ferridriver has always given a script one,
/// and what it names is this module's own surface — the same object an
/// `import` of `node:fs` answers with, so the two cannot drift.
///
/// # Errors
///
/// Returns an error if the global cannot be defined.
pub fn init(ctx: &Ctx<'_>) -> Result<()> {
    ctx.globals().set("fs", fs_object(ctx)?)
}

/// Export every member of `namespace`, plus `namespace` itself as
/// `default`.
///
/// Unlike `export_default`, the object handed out IS the one passed in,
/// so an import and the global stay the same object.
fn export_object<'js>(exports: &Exports<'js>, namespace: &Object<'js>) -> Result<()> {
    for name in namespace.keys::<String>() {
        let name = name?;
        let value: Value<'js> = namespace.get(&name)?;
        exports.export(name, value)?;
    }
    exports.export("default", namespace.clone())?;
    Ok(())
}
