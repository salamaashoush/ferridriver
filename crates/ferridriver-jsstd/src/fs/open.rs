// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0
use std::path::PathBuf;

use crate::utils::result::ResultExt;
use rquickjs::{function::Opt, Ctx, Exception, Result};
use tokio::fs::OpenOptions;

use super::file_handle::FileHandle;

pub async fn open(
    ctx: Ctx<'_>,
    path: String,
    flags: Opt<String>,
    mode: Opt<u32>,
) -> Result<FileHandle> {
    let mut options = OpenOptions::new();
    match flags.0.as_deref().unwrap_or("r") {
        // We are not supporting the sync modes
        "a" => options.append(true).create(true),
        "ax" => options.append(true).create_new(true),
        "a+" => options.append(true).read(true),
        "r" => options.read(true),
        "r+" => options.read(true).write(true),
        "w" => options.write(true).create(true).truncate(true),
        "wx" => options.write(true).create_new(true),
        "w+" => options.write(true).read(true).create(true).truncate(true),
        "wx+" => options.write(true).read(true).create_new(true),
        flags => {
            return Err(Exception::throw_message(
                &ctx,
                &["Invalid flags '", flags, "'"].concat(),
            ))
        },
    };
    #[cfg(unix)]
    {
        let mode = mode.0.unwrap_or(0o666);
        options.mode(mode);
    }
    #[cfg(not(unix))]
    {
        _ = mode;
    }

    let path = PathBuf::from(path);
    let file = options
        .open(&path)
        .await
        .or_throw_msg(&ctx, "Cannot open file")?;

    Ok(FileHandle::new(file, path))
}
