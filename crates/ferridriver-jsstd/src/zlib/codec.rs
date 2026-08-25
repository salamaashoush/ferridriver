// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0
use crate::buffer::Buffer;
use crate::context::CtxExtension;
use crate::utils::{bytes::ObjectBytes, object::ObjectExt, result::ResultExt};
use rquickjs::{
    prelude::{Opt, Rest},
    Ctx, Error, Exception, Function, IntoJs, Null, Result, Value,
};

use crate::{define_cb_function, define_sync_function};
use super::{max_output_length, read_to_end_limited};

enum ZlibCommand {
    Deflate,
    DeflateRaw,
    Gzip,
    Inflate,
    InflateRaw,
    Gunzip,
}

fn zlib_converter<'js>(
    ctx: Ctx<'js>,
    bytes: ObjectBytes<'js>,
    options: Opt<Value<'js>>,
    command: ZlibCommand,
) -> Result<Value<'js>> {
    let src = bytes.as_bytes(&ctx)?;

    let mut level = crate::compression::zlib::Compression::default();
    if let Some(options) = options.0.as_ref() {
        if let Some(opt) = options.get_optional("level")? {
            level = crate::compression::zlib::Compression::new(opt);
        }
    }
    let limit = max_output_length(&options)?;

    let dst = match command {
        ZlibCommand::Deflate => read_to_end_limited(
            &ctx,
            crate::compression::zlib::encoder(src, level),
            limit,
            src.len(),
        )?,
        ZlibCommand::DeflateRaw => read_to_end_limited(
            &ctx,
            crate::compression::deflate::encoder(src, level),
            limit,
            src.len(),
        )?,
        ZlibCommand::Gzip => read_to_end_limited(
            &ctx,
            crate::compression::gz::encoder(src, level),
            limit,
            src.len(),
        )?,
        ZlibCommand::Inflate => {
            read_to_end_limited(&ctx, crate::compression::zlib::decoder(src), limit, src.len())?
        },
        ZlibCommand::InflateRaw => read_to_end_limited(
            &ctx,
            crate::compression::deflate::decoder(src),
            limit,
            src.len(),
        )?,
        ZlibCommand::Gunzip => {
            read_to_end_limited(&ctx, crate::compression::gz::decoder(src), limit, src.len())?
        },
    };

    Buffer(dst).into_js(&ctx)
}

define_cb_function!(deflate, zlib_converter, ZlibCommand::Deflate);
define_sync_function!(deflate_sync, zlib_converter, ZlibCommand::Deflate);

define_cb_function!(deflate_raw, zlib_converter, ZlibCommand::DeflateRaw);
define_sync_function!(deflate_raw_sync, zlib_converter, ZlibCommand::DeflateRaw);

define_cb_function!(gzip, zlib_converter, ZlibCommand::Gzip);
define_sync_function!(gzip_sync, zlib_converter, ZlibCommand::Gzip);

define_cb_function!(inflate, zlib_converter, ZlibCommand::Inflate);
define_sync_function!(inflate_sync, zlib_converter, ZlibCommand::Inflate);

define_cb_function!(inflate_raw, zlib_converter, ZlibCommand::InflateRaw);
define_sync_function!(inflate_raw_sync, zlib_converter, ZlibCommand::InflateRaw);

define_cb_function!(gunzip, zlib_converter, ZlibCommand::Gunzip);
define_sync_function!(gunzip_sync, zlib_converter, ZlibCommand::Gunzip);
