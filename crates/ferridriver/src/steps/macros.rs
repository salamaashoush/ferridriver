/// Declare a step definition with metadata + async handler in one block.
macro_rules! step {
    (
        $name:ident {
            category: $cat:expr,
            pattern: $pat:expr,
            description: $desc:expr,
            example: $ex:expr,
            execute($page:ident, $caps:ident, $table:ident, $vars:ident) $body:block
        }
    ) => {
        pub struct $name;

        impl $name {
            fn compiled_regex() -> &'static ::regex::Regex {
                static RE: ::std::sync::OnceLock<::regex::Regex> = ::std::sync::OnceLock::new();
                RE.get_or_init(|| ::regex::Regex::new($pat).unwrap())
            }
        }

        #[::async_trait::async_trait]
        impl $crate::steps::StepDef for $name {
            fn description(&self) -> &'static str { $desc }
            fn category(&self) -> $crate::steps::StepCategory { $cat }
            fn example(&self) -> &'static str { $ex }
            fn pattern(&self) -> &::regex::Regex { Self::compiled_regex() }

            async fn execute(
                &self,
                $page: &::std::sync::Arc<$crate::page::Page>,
                $caps: &::regex::Captures<'_>,
                $table: Option<&[Vec<String>]>,
                $vars: &mut ::rustc_hash::FxHashMap<String, String>,
            ) -> Result<Option<::serde_json::Value>, String> $body
        }
    };
}
