// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0
use crate::utils::module::{export_default, ModuleInfo};
use rquickjs::{
    module::{Declarations, Exports, ModuleDef},
    Ctx, Object, Result,
};


pub struct PerfHooksModule;

impl ModuleDef for PerfHooksModule {
    fn declare(declare: &Declarations<'_>) -> Result<()> {
        declare.declare("performance")?;
        declare.declare("default")?;
        Ok(())
    }

    fn evaluate<'js>(ctx: &Ctx<'js>, exports: &Exports<'js>) -> Result<()> {
        export_default(ctx, exports, |default| {
            let globals = ctx.globals();

            let performance: Object = globals.get("performance")?;
            default.set("performance", performance)?;

            Ok(())
        })
    }
}

impl From<PerfHooksModule> for ModuleInfo<PerfHooksModule> {
    fn from(val: PerfHooksModule) -> Self {
        ModuleInfo {
            name: "perf_hooks",
            module: val,
        }
    }
}
