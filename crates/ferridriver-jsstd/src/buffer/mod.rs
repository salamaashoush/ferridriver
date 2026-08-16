//! Vendored `llrt_buffer`, minus its `Blob` and `File` — ferridriver has
//! its own of each in `ferridriver-script`, and a second implementation is
//! exactly what this crate exists to avoid.

// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0
use crate::utils::{
    module::{export_default, ModuleInfo},
    object::define_subclass,
    primordials::{BasePrimordials, Primordial},
};
use rquickjs::{
    function::{Args, Constructor, Rest},
    module::{Declarations, Exports, ModuleDef},
    Ctx, Function, IntoJs, Object, Result, Value,
};

pub use self::array_buffer_view::*;
pub use self::class::*;

mod array_buffer_view;
mod class;

pub struct BufferModule;

impl ModuleDef for BufferModule {
    fn declare(declare: &Declarations) -> Result<()> {
        declare.declare(stringify!(Buffer))?;
        declare.declare("atob")?;
        declare.declare("btoa")?;
        declare.declare("constants")?;
        declare.declare("default")?;
        Ok(())
    }

    fn evaluate<'js>(ctx: &Ctx<'js>, exports: &Exports<'js>) -> Result<()> {
        let globals = ctx.globals();
        let buf: Constructor = globals.get(stringify!(Buffer))?;

        let constants = Object::new(ctx.clone())?;
        constants.set("MAX_LENGTH", u32::MAX)?; // For QuickJS
        constants.set("MAX_STRING_LENGTH", (1 << 30) - 1)?; // For QuickJS

        let atob: Function = ctx.globals().get("atob")?;
        let btoa: Function = ctx.globals().get("btoa")?;

        export_default(ctx, exports, |default| {
            default.set(stringify!(Buffer), buf)?;
            default.set("atob", atob.into_js(ctx)?)?;
            default.set("btoa", btoa.into_js(ctx)?)?;
            default.set("constants", constants)?;
            Ok(())
        })?;

        Ok(())
    }
}

impl From<BufferModule> for ModuleInfo<BufferModule> {
    fn from(val: BufferModule) -> Self {
        ModuleInfo {
            name: "buffer",
            module: val,
        }
    }
}

pub fn init<'js>(ctx: &Ctx<'js>) -> Result<()> {
    BasePrimordials::init(ctx)?;

    // Buffer extends the native Uint8Array: it forwards construction to the
    // Uint8Array constructor and inherits its static and prototype members.
    let uint8array = BasePrimordials::get(ctx)?.constructor_uint8array.clone();
    let buffer_ctor = define_subclass(
        ctx,
        stringify!(Buffer),
        &uint8array,
        |ctx: Ctx<'js>, args: Rest<Value<'js>>| {
            let uint8array = &BasePrimordials::get(&ctx)?.constructor_uint8array;
            let mut ctor_args = Args::new(ctx.clone(), args.0.len());
            ctor_args.push_args(args.0)?;
            ctor_args.construct::<Value>(uint8array)
        },
    )?;
    let buffer: Object = buffer_ctor.into_value().into_object().unwrap();
    set_prototype(ctx, buffer)?;

    BufferPrimordials::init(ctx)?;

    // Local delta: `equals` and `toJSON` are Node `Buffer` methods
    // upstream does not define, and the implementation this vendoring
    // replaced had both. Everything else on the prototype comes from
    // upstream or from `Uint8Array`.
    add_missing_prototype_methods(ctx)?;

    //init primordials
    let _ = BufferPrimordials::get(ctx)?;

    Ok(())
}

/// Node `Buffer` prototype methods `llrt_buffer` does not define.
fn add_missing_prototype_methods<'js>(ctx: &Ctx<'js>) -> Result<()> {
    let buffer: Constructor<'js> = ctx.globals().get(stringify!(Buffer))?;
    let prototype: Object<'js> = buffer.get(rquickjs::atom::PredefinedAtom::Prototype)?;

    prototype.set(
        "equals",
        Function::new(ctx.clone(), |this: rquickjs::function::This<Object<'js>>, other: Object<'js>| -> Result<bool> {
            let (a, b) = (bytes_of(&this.0)?, bytes_of(&other)?);
            Ok(a == b)
        })?,
    )?;

    prototype.set(
        "toJSON",
        Function::new(ctx.clone(), |ctx: Ctx<'js>, this: rquickjs::function::This<Object<'js>>| -> Result<Object<'js>> {
            let json = Object::new(ctx.clone())?;
            json.set("type", "Buffer")?;
            json.set("data", bytes_of(&this.0)?)?;
            Ok(json)
        })?,
    )?;

    Ok(())
}

/// The bytes behind a `Buffer` (or any `Uint8Array` view).
fn bytes_of<'js>(object: &Object<'js>) -> Result<Vec<u8>> {
    match crate::utils::bytes::ObjectBytes::from_array_buffer(object)? {
        Some(bytes) => Ok(bytes.as_bytes(object.ctx())?.to_vec()),
        None => Ok(Vec::new()),
    }
}
