// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0
//! `TextEncoder` / `TextDecoder` and their stream forms, from upstream
//! `llrt_util`. Only the codecs are taken: `format`, `inherits`,
//! `styleText` and `inspect` are `crate::node::util`'s.

pub mod text_decoder;
pub mod text_decoder_stream;
pub mod text_encoder;
pub mod text_encoder_stream;

use rquickjs::{Class, Ctx, Result};

pub use self::text_decoder::TextDecoder;
pub use self::text_decoder_stream::TextDecoderStream;
pub use self::text_encoder::TextEncoder;
pub use self::text_encoder_stream::TextEncoderStream;

pub fn init(ctx: &Ctx<'_>) -> Result<()> {
    let globals = ctx.globals();

    Class::<TextEncoder>::define(&globals)?;
    Class::<TextDecoder>::define(&globals)?;
    Class::<TextEncoderStream>::define(&globals)?;
    Class::<TextDecoderStream>::define(&globals)?;

    Ok(())
}
