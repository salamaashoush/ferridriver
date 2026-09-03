//! Finding objects by what they look like rather than by id.
//!
//! Every other query here starts from a node someone already has. This
//! is the one that finds it: a scan over the whole node array keeping
//! whatever matches a name, a type, a property it carries, a size band
//! or an attachment state, ported from `HeapSnapshot.ts::queryObjects`.
//!
//! # Where the pattern language differs, and why it is not a divergence
//!
//! `className` and `propertyName` are regular expressions, matched
//! case-insensitively and unanchored, and upstream compiles them with
//! JavaScript's engine. This uses Rust's, which has no backreferences
//! and no lookaround. A pattern using either is REJECTED here rather
//! than quietly matching something else: the two engines agree on
//! everything they both accept, and a caller finds out when they do
//! not.

use serde::{Deserialize, Serialize};

use crate::analysis::Analysis;
use crate::error::{HeapError, Result};
use crate::query::NodeSummary;

/// `DOMLinkState.DETACHED`.
const DETACHED: u64 = 2;

/// Which order the matches come back in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum QuerySort {
  /// Heaviest first, which is what a leak hunt wants and what
  /// `query_heapsnapshot_objects` defaults to.
  #[default]
  RetainedSize,
  SelfSize,
  /// Oldest first. Ids ascend with allocation order, so this is the one
  /// order that means the same thing in two snapshots.
  Id,
}

/// What to keep. Every field left unset is a filter not applied.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ObjectQuery {
  pub class_name: Option<String>,
  pub property_name: Option<String>,
  pub node_type: Option<String>,
  pub min_retained_size: Option<u64>,
  pub max_retained_size: Option<u64>,
  pub min_self_size: Option<u64>,
  pub max_self_size: Option<u64>,
  pub is_detached: Option<bool>,
  pub sort_by: Option<QuerySort>,
}

impl Analysis {
  /// Every object matching the query, in the order it asked for.
  ///
  /// # Errors
  ///
  /// [`HeapError::Pattern`] when `class_name` or `property_name` is not
  /// a regular expression this engine can compile.
  pub fn query_objects(&self, query: &ObjectQuery) -> Result<Vec<NodeSummary>> {
    let class_name = compile(query.class_name.as_deref(), "className")?;
    let property_name = compile(query.property_name.as_deref(), "propertyName")?;
    let node_type = query.node_type.as_ref().map(|kind| kind.to_lowercase());

    let snapshot = &self.snapshot;
    let mut matched: Vec<usize> = Vec::new();
    for ordinal in 0..snapshot.node_count {
      let self_size = snapshot.node_self_size(ordinal);
      if query.min_self_size.is_some_and(|least| self_size < least)
        || query.max_self_size.is_some_and(|most| self_size > most)
      {
        continue;
      }
      let retained = self.retained_size(ordinal);
      if query.min_retained_size.is_some_and(|least| retained < least)
        || query.max_retained_size.is_some_and(|most| retained > most)
      {
        continue;
      }
      if query
        .is_detached
        .is_some_and(|wanted| (snapshot.node_detachedness(ordinal) == DETACHED) != wanted)
      {
        continue;
      }
      if let Some(pattern) = &class_name
        && !pattern.is_match(&self.node_name(ordinal))
      {
        continue;
      }
      if let Some(wanted) = &node_type
        && snapshot.node_type_name(ordinal).to_lowercase() != *wanted
      {
        continue;
      }
      if let Some(pattern) = &property_name
        && !(snapshot.first_edge_index[ordinal]..snapshot.first_edge_index[ordinal + 1])
          .any(|edge| pattern.is_match(&self.edge_display_name(edge)))
      {
        continue;
      }
      matched.push(ordinal);
    }

    // Stable throughout, so equal sizes stay in the order the heap
    // holds them rather than in whichever order a sort happened to
    // leave them.
    match query.sort_by.unwrap_or_default() {
      QuerySort::RetainedSize => matched.sort_by_key(|&ordinal| std::cmp::Reverse(self.retained_size(ordinal))),
      QuerySort::SelfSize => {
        matched.sort_by_key(|&ordinal| std::cmp::Reverse(snapshot.node_self_size(ordinal)));
      },
      QuerySort::Id => matched.sort_by_key(|&ordinal| snapshot.node_id(ordinal)),
    }

    Ok(matched.into_iter().map(|ordinal| self.node_summary(ordinal)).collect())
  }
}

fn compile(pattern: Option<&str>, what: &'static str) -> Result<Option<regex::Regex>> {
  pattern
    .map(|pattern| {
      regex::RegexBuilder::new(pattern)
        .case_insensitive(true)
        .build()
        .map_err(|source| HeapError::Pattern {
          what,
          source: Box::new(source),
        })
    })
    .transpose()
}
