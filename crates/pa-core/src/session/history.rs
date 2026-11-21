//! The history view: one ordered walk from the root to the leaf that applies
//! every substitution on the way — compaction summaries and model-authored
//! context edits — and yields the entries the request is built from.

use std::collections::{HashMap, HashSet};

use pa_types::ai::{
    AssistantContentBlock, AssistantMessage, TextContent, ToolResultMessage, UserContent,
    UserMessage,
};
use pa_types::session::{
    AgentMessage, BranchSummaryMessage, CompactionEntry, CompactionSummaryMessage, ContextAnchor,
    ContextArchive, ContextChange, ContextEditEntry, ContextRange, ContextRangeEnd, ContextRole,
    ContextSelector, ContextTarget, FileEntry,
};

use super::{create_custom_message, timestamp_to_millis};
use crate::session_engine::compaction::estimate_tokens;

/// One entry of the model-facing history view.
#[derive(Debug, Clone)]
pub struct HistoryEntry {
    /// Source entry id; `None` for an inserted entry, which lives inside the
    /// edit row that added it and has no identity of its own.
    pub id: Option<String>,
    /// Index into the session entries for a record-backed entry.
    pub source: Option<usize>,
    pub role: ContextRole,
    pub tokens: u64,
    pub text: String,
    /// The message this entry contributes to the request; `None` for notes,
    /// which never reach the provider.
    pub message: Option<AgentMessage>,
    /// 1-based user turn the entry belongs to.
    pub turn: usize,
    /// Position in the session's recorded order; stable across later edits, so
    /// a view sort can fall back to it.
    pub seq: usize,
}

/// What one applied edit removed, replaced, and inserted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EditCounts {
    pub dropped: usize,
    pub replaced: usize,
    pub inserted: usize,
}

/// How far the newest provider-reported usage has drifted from what the view
/// now costs, because a context edit changed what came before it. Entries a
/// replaced or dropped row contributed are estimated per entry, so the
/// correction is bounded by that entry's own estimate.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UsageDrift {
    /// Signed token delta from edits applied above the newest usage anchor.
    pub tokens: i64,
    /// Set when the anchor itself stopped being sent, so no observed usage
    /// describes this view any more.
    pub anchor_lost: bool,
    /// Timestamp of the newest compaction on the path. Usage reported at or
    /// before it describes a view this request no longer sends, and neither
    /// does any correction computed above it.
    pub boundary: Option<u64>,
}

impl UsageDrift {
    #[must_use]
    pub fn is_clean(self) -> bool {
        self.tokens == 0 && !self.anchor_lost
    }
}

/// The state one root-to-leaf walk carries: the live view plus the settings
/// resolved on the same pass.
#[derive(Debug, Clone)]
pub struct HistoryWalk {
    /// The live view, in request order.
    pub entries: Vec<HistoryEntry>,
    /// Record-backed entries the walk stopped showing (dropped by an edit or
    /// covered by a compaction).
    pub hidden: Vec<HistoryEntry>,
    pub thinking_level: String,
    pub service_tier: Option<pa_types::ai::ServiceTier>,
    pub model: Option<(String, String)>,
    pub archive: Option<ContextArchive>,
    path: Vec<usize>,
    path_position: HashMap<usize, usize>,
    path_ids: HashSet<String>,
    session_ids: HashSet<String>,
    turn: usize,
    next_seq: usize,
    /// The compaction entry whose archive this walk accepted.
    archive_source: Option<usize>,
    /// Edits on the path that no longer resolved and were left out of the view.
    pub skipped_edits: usize,
    /// Correction the newest usage anchor needs because edits changed the view.
    pub usage_drift: UsageDrift,
}

/// Walk the root-to-leaf path and apply every substitution on it.
#[must_use]
pub fn walk_history(entries: &[FileEntry], leaf_id: Option<&str>) -> HistoryWalk {
    let by_id: HashMap<&str, usize> = entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| entry.id().map(|id| (id, index)))
        .collect();
    let leaf_index = match leaf_id {
        Some("") | None => entries.len().checked_sub(1),
        Some(id) => by_id.get(id).copied(),
    };
    let mut walk = HistoryWalk {
        entries: Vec::new(),
        hidden: Vec::new(),
        thinking_level: "off".to_string(),
        service_tier: None,
        model: None,
        archive: None,
        path: Vec::new(),
        path_position: HashMap::new(),
        path_ids: HashSet::new(),
        session_ids: entries
            .iter()
            .filter_map(|entry| entry.id().map(str::to_owned))
            .collect(),
        turn: 0,
        next_seq: 0,
        archive_source: None,
        skipped_edits: 0,
        usage_drift: UsageDrift::default(),
    };
    let Some(leaf_index) = leaf_index else {
        return walk;
    };
    let mut visited = HashSet::new();
    let mut current = Some(leaf_index);
    while let Some(index) = current {
        if !visited.insert(index) {
            break;
        }
        walk.path.push(index);
        current = entries[index]
            .parent_id()
            .and_then(|parent_id| by_id.get(parent_id).copied());
    }
    walk.path.reverse();
    walk.path_position = walk
        .path
        .iter()
        .enumerate()
        .map(|(position, index)| (*index, position))
        .collect();
    walk.path_ids = walk
        .path
        .iter()
        .filter_map(|index| entries[*index].id().map(str::to_owned))
        .collect();
    let path = walk.path.clone();
    for index in path {
        walk.visit(entries, index);
    }
    walk
}

impl HistoryWalk {
    /// The messages the request is built from: notes never cross to the
    /// provider.
    #[must_use]
    pub fn messages(&self) -> Vec<AgentMessage> {
        self.entries
            .iter()
            .filter_map(|entry| entry.message.clone())
            .collect()
    }

    /// A record-backed entry by id, live or dropped from the live view.
    #[must_use]
    pub fn entry_by_id(&self, id: &str) -> Option<&HistoryEntry> {
        self.entries
            .iter()
            .chain(self.hidden.iter())
            .find(|entry| entry.id.as_deref() == Some(id))
    }

    /// Resolve one target against the live view, returning its positions.
    ///
    /// # Errors
    ///
    /// Returns the rule and the target that rejected the selector.
    pub fn resolve(&self, target: &ContextTarget) -> Result<Vec<usize>, String> {
        self.resolve_target(&self.entries, target)
    }

    fn visit(&mut self, entries: &[FileEntry], index: usize) {
        match &entries[index] {
            FileEntry::ThinkingLevelChange { payload, .. } => {
                self.thinking_level.clone_from(&payload.thinking_level);
            }
            FileEntry::ServiceTierChange { payload, .. } => {
                self.service_tier = payload.service_tier;
            }
            FileEntry::ModelChange { payload, .. } => {
                self.model = Some((payload.provider.clone(), payload.model_id.clone()));
            }
            FileEntry::Message {
                message: AgentMessage::Assistant(assistant),
                ..
            } => {
                self.model = Some((assistant.provider.clone(), assistant.model.clone()));
                self.push(entries, index);
            }
            FileEntry::Compaction { payload, .. } => {
                let payload = payload.clone();
                self.apply_compaction(entries, index, &payload);
            }
            FileEntry::ContextEdit { payload, .. } => {
                let payload = payload.clone();
                let timestamp = timestamp_to_millis(entries[index].timestamp());
                if self.apply(&payload, timestamp).is_err() {
                    self.skipped_edits += 1;
                }
            }
            _ => self.push(entries, index),
        }
    }

    fn push(&mut self, entries: &[FileEntry], index: usize) {
        let Some(message) = message_from_entry(&entries[index]) else {
            return;
        };
        let id = entries[index].id().map(str::to_owned);
        self.push_message(id, Some(index), message);
    }

    fn push_message(&mut self, id: Option<String>, source: Option<usize>, message: AgentMessage) {
        if crate::session_engine::compaction::has_usage(&message) {
            // A fresh observation describes the view as it stands now, edits and
            // all, so the correction the previous anchor needed no longer applies.
            // The compaction boundary stays: usage reported before it is still
            // not about this request.
            self.usage_drift = UsageDrift {
                boundary: self.usage_drift.boundary,
                ..UsageDrift::default()
            };
        }
        let role = role_of(&message);
        let turn = self.advance_turn(role);
        let seq = self.next_seq;
        self.next_seq += 1;
        self.entries.push(HistoryEntry {
            id,
            source,
            role,
            tokens: estimate_tokens(&message),
            text: message_text(&message),
            message: Some(message),
            turn,
            seq,
        });
    }

    fn advance_turn(&mut self, role: ContextRole) -> usize {
        if role == ContextRole::User {
            self.turn += 1;
        }
        self.turn.max(1)
    }

    fn apply_compaction(&mut self, entries: &[FileEntry], index: usize, payload: &CompactionEntry) {
        self.archive.clone_from(&payload.context_archive);
        self.archive_source = Some(index);
        let portable = self.archive.as_ref().is_none_or(|archive| {
            matches!(
                archive.representation,
                pa_types::session::ContextRepresentation::Standard { .. }
            )
        });
        let covered: Option<HashSet<&str>> = self.archive.as_ref().map(|archive| {
            archive
                .covered_entry_ids
                .iter()
                .map(String::as_str)
                .collect()
        });
        let first_kept = payload.first_kept_entry_id.as_deref().and_then(|id| {
            self.path
                .iter()
                .position(|other| entries[*other].id() == Some(id))
        });
        let mut retained = Vec::new();
        for entry in std::mem::take(&mut self.entries) {
            let source = entry.source;
            let keep = match source {
                // An inserted note is not a covered record; it survives.
                None => true,
                Some(source) => match (&covered, first_kept) {
                    (Some(covered), _) => {
                        entry.id.as_deref().is_some_and(|id| !covered.contains(id))
                    }
                    (None, Some(first_kept)) => self
                        .path_position
                        .get(&source)
                        .is_some_and(|position| *position >= first_kept),
                    (None, None) => false,
                },
            };
            if keep {
                retained.push(entry);
            } else {
                self.hidden.push(entry);
            }
        }
        let retained_messages = retained
            .iter()
            .filter(|entry| entry.message.is_some())
            .count();
        self.turn = 0;
        self.entries = Vec::new();
        if !portable && self.archive.is_some() {
            // A native archive is expanded at the stream boundary, so its
            // summary is part of what the model reads without being one message
            // in the view: expose it as a view-only entry, so the model can
            // address it and dropping it clears the archive.
            let turn = self.advance_turn(ContextRole::Assistant);
            let summary = payload.summary.clone();
            self.entries.push(HistoryEntry {
                id: entries[index].id().map(str::to_owned),
                source: Some(index),
                role: ContextRole::Assistant,
                tokens: text_tokens(&summary),
                text: summary,
                message: None,
                turn,
                seq: self.next_seq,
            });
            self.next_seq += 1;
            self.turn = 0;
        }
        if portable {
            let message = AgentMessage::CompactionSummary(CompactionSummaryMessage {
                rest: pa_types::JsonMap::new(),
                summary: payload.summary.clone(),
                tokens_before: payload.tokens_before,
                retained_message_count: Some(retained_messages as u64),
                custom_instructions: payload.custom_instructions.clone(),
                timestamp: timestamp_to_millis(entries[index].timestamp()),
            });
            let id = entries[index].id().map(str::to_owned);
            self.push_message(id, Some(index), message);
        }
        for entry in retained {
            let turn = self.advance_turn(entry.role);
            self.entries.push(HistoryEntry { turn, ..entry });
        }
        // Every usage observation and every edit correction so far is about the
        // prefix this compaction just replaced.
        self.usage_drift = UsageDrift {
            boundary: Some(timestamp_to_millis(entries[index].timestamp())),
            ..UsageDrift::default()
        };
    }

    /// Apply one edit to the live view. Every selector resolves against the
    /// view as it stood before this call, so no change can invalidate another.
    /// Nothing is mutated when the edit is rejected.
    ///
    /// # Errors
    ///
    /// Returns the rule and the target that rejected the edit: a tool result
    /// dropped without its call, a changed assistant tool call, a selector off
    /// the current path, a range spanning branches, an unknown op, or a key the
    /// op does not use.
    pub fn apply(&mut self, edit: &ContextEditEntry, timestamp: u64) -> Result<EditCounts, String> {
        let snapshot = self.entries.clone();
        let anchor = last_usage_anchor(&snapshot);
        let mut drift = self.usage_drift;
        let mut anchor_lost = drift.anchor_lost;
        let mut dropped: HashSet<usize> = HashSet::new();
        let mut replaced: HashMap<usize, HistoryEntry> = HashMap::new();
        let mut inserted: Vec<(usize, HistoryEntry)> = Vec::new();
        for change in &edit.changes {
            match change {
                ContextChange::Drop { at } => {
                    for position in self.resolve_target(&snapshot, at)? {
                        if replaced.contains_key(&position) {
                            return Err(format!(
                                "context_edit drop and replace name the same entry: {}",
                                describe(&snapshot[position], position)
                            ));
                        }
                        if snapshot[position].message.is_some() {
                            if anchor.is_some_and(|anchor| position == anchor) {
                                anchor_lost = true;
                            } else if anchor.is_some_and(|anchor| position < anchor) {
                                drift.tokens -= token_delta(&snapshot[position]);
                            }
                        }
                        dropped.insert(position);
                    }
                }
                ContextChange::Replace { at, text } => {
                    let position = self.resolve_point(&snapshot, at)?;
                    if dropped.contains(&position) {
                        return Err(format!(
                            "context_edit drop and replace name the same entry: {}",
                            describe(&snapshot[position], position)
                        ));
                    }
                    let entry = &snapshot[position];
                    if entry.role == ContextRole::Assistant
                        && assistant_has_tool_calls(entry.message.as_ref())
                    {
                        return Err(format!(
                            "context_edit replaces the tool calls of assistant entry {}",
                            entry.id.as_deref().unwrap_or("<inserted>")
                        ));
                    }
                    let mut replacement = entry.clone();
                    replacement.text.clone_from(text);
                    replacement.message =
                        replaced_message(replacement.role, text, entry.message.as_ref());
                    replacement.tokens = replacement
                        .message
                        .as_ref()
                        .map_or_else(|| text_tokens(&replacement.text), estimate_tokens);
                    if replacement.message.is_some() {
                        if anchor.is_some_and(|anchor| position == anchor) {
                            anchor_lost = true;
                        } else if anchor.is_some_and(|anchor| position < anchor) {
                            drift.tokens += token_delta(&replacement) - token_delta(entry);
                        }
                    }
                    replaced.insert(position, replacement);
                }
                ContextChange::Insert { text, role, after } => {
                    if !matches!(role, ContextRole::User | ContextRole::Notes) {
                        return Err(format!(
                            "context_edit insert cannot create a '{}' entry; insert user or notes",
                            role.as_str()
                        ));
                    }
                    let after_position = self.resolve_anchor(&snapshot, after)?;
                    let entry = inserted_entry(
                        *role,
                        text,
                        after_position,
                        &snapshot,
                        self.turn.max(1),
                        timestamp,
                    );
                    if anchor.is_some_and(|anchor| after_position.map_or(0, |at| at + 1) <= anchor)
                        && entry.message.is_some()
                    {
                        drift.tokens += i64::from(u32::try_from(entry.tokens).unwrap_or(u32::MAX));
                    }
                    inserted.push((after_position.map_or(0, |position| position + 1), entry));
                }
            }
        }
        inserted.sort_by_key(|(position, _)| *position);
        let mut view = Vec::with_capacity(snapshot.len() + inserted.len());
        let mut hidden = Vec::new();
        let mut counts = EditCounts::default();
        let mut cursor = 0usize;
        for (position, entry) in snapshot.iter().enumerate() {
            while cursor < inserted.len() && inserted[cursor].0 == position {
                view.push(inserted[cursor].1.clone());
                counts.inserted += 1;
                cursor += 1;
            }
            if dropped.contains(&position) {
                hidden.push(entry.clone());
                counts.dropped += 1;
                continue;
            }
            if let Some(entry) = replaced.get(&position) {
                counts.replaced += 1;
                view.push(entry.clone());
                continue;
            }
            view.push(entry.clone());
        }
        while cursor < inserted.len() {
            view.push(inserted[cursor].1.clone());
            counts.inserted += 1;
            cursor += 1;
        }
        check_tool_calls(&snapshot, &view, &dropped)?;
        let touched: HashSet<usize> = dropped
            .iter()
            .chain(replaced.keys())
            .filter_map(|position| snapshot[*position].source)
            .collect();
        let archive_touched = self
            .archive_source
            .is_some_and(|source| touched.contains(&source));
        if archive_touched {
            // The archive's own row left the view, so the prefix it expanded is
            // gone with it and no provider observation describes what remains.
            anchor_lost = true;
        }
        self.hidden.extend(hidden);
        self.entries = view;
        self.usage_drift = UsageDrift {
            tokens: if anchor_lost { 0 } else { drift.tokens },
            anchor_lost,
            boundary: self.usage_drift.boundary,
        };
        if archive_touched {
            self.archive = None;
            self.archive_source = None;
        }
        Ok(counts)
    }

    fn resolve_point(
        &self,
        snapshot: &[HistoryEntry],
        selector: &ContextSelector,
    ) -> Result<usize, String> {
        match selector {
            ContextSelector::EntryId(id) => {
                position_of(snapshot, id).ok_or_else(|| self.off_path(id))
            }
            ContextSelector::Position(position) => {
                normalize(*position, snapshot.len()).ok_or_else(|| {
                    format!(
                        "context_edit selector off the current path: position {position} is outside the live view of {} entries",
                        snapshot.len()
                    )
                })
            }
        }
    }

    fn resolve_target(
        &self,
        snapshot: &[HistoryEntry],
        target: &ContextTarget,
    ) -> Result<Vec<usize>, String> {
        match target {
            ContextTarget::Point(selector) => Ok(vec![self.resolve_point(snapshot, selector)?]),
            ContextTarget::Range(range) => self.resolve_range(snapshot, range),
        }
    }

    fn resolve_range(
        &self,
        snapshot: &[HistoryEntry],
        range: &ContextRange,
    ) -> Result<Vec<usize>, String> {
        let start = self.resolve_range_end(snapshot, &range.start, 0)?;
        let end = self.resolve_range_end(snapshot, &range.end, snapshot.len())?;
        if start > end {
            return Err(format!(
                "context_edit range end precedes its start: {start}..{end}"
            ));
        }
        Ok((start..end).collect())
    }

    fn resolve_range_end(
        &self,
        snapshot: &[HistoryEntry],
        end: &ContextRangeEnd,
        path_bound: usize,
    ) -> Result<usize, String> {
        match end {
            ContextRangeEnd::PathBound => Ok(path_bound),
            ContextRangeEnd::EntryId(id) => {
                position_of(snapshot, id).ok_or_else(|| self.off_path(id))
            }
            ContextRangeEnd::Position(position) => {
                normalize_bound(*position, snapshot.len()).ok_or_else(|| {
                    format!(
                        "context_edit selector off the current path: position {position} is outside the live view of {} entries",
                        snapshot.len()
                    )
                })
            }
        }
    }

    fn resolve_anchor(
        &self,
        snapshot: &[HistoryEntry],
        anchor: &ContextAnchor,
    ) -> Result<Option<usize>, String> {
        match anchor {
            ContextAnchor::Position(position) => {
                if snapshot.is_empty() {
                    return Ok(None);
                }
                let index = normalize(*position, snapshot.len()).ok_or_else(|| {
                    format!(
                        "context_edit insert 'after' off the current path: position {position} is outside the live view of {} entries",
                        snapshot.len()
                    )
                })?;
                Ok(Some(index))
            }
            ContextAnchor::EntryId(id) => position_of(snapshot, id)
                .map(Some)
                .ok_or_else(|| self.off_path(id)),
        }
    }

    fn off_path(&self, id: &str) -> String {
        if self.path_ids.contains(id) {
            return format!("context_edit selector off the current path: {id} is no longer live");
        }
        if self.session_ids.contains(id) {
            return format!(
                "context_edit selector off the current path: {id} is on another branch"
            );
        }
        format!("context_edit selector off the current path: {id} is not in this session")
    }
}

/// The newest view position whose message carries provider-reported usage.
fn last_usage_anchor(snapshot: &[HistoryEntry]) -> Option<usize> {
    snapshot.iter().rposition(|entry| {
        entry
            .message
            .as_ref()
            .is_some_and(crate::session_engine::compaction::has_usage)
    })
}

fn token_delta(entry: &HistoryEntry) -> i64 {
    i64::from(u32::try_from(entry.tokens).unwrap_or(u32::MAX))
}

/// Name an entry in a rejection: its id, or its view position for an inserted row.
fn describe(entry: &HistoryEntry, position: usize) -> String {
    entry
        .id
        .clone()
        .unwrap_or_else(|| format!("position {position}"))
}

fn position_of(snapshot: &[HistoryEntry], id: &str) -> Option<usize> {
    snapshot
        .iter()
        .position(|entry| entry.id.as_deref() == Some(id))
}

fn assistant_has_tool_calls(message: Option<&AgentMessage>) -> bool {
    let Some(AgentMessage::Assistant(assistant)) = message else {
        return false;
    };
    assistant
        .content
        .iter()
        .any(|block| matches!(block, AssistantContentBlock::ToolCall(_)))
}

fn check_tool_calls(
    snapshot: &[HistoryEntry],
    view: &[HistoryEntry],
    dropped: &HashSet<usize>,
) -> Result<(), String> {
    let surviving_calls: HashSet<&str> = view
        .iter()
        .filter_map(|entry| entry.message.as_ref())
        .filter_map(|message| match message {
            AgentMessage::Assistant(assistant) => Some(assistant),
            _ => None,
        })
        .flat_map(|assistant| assistant.content.iter())
        .filter_map(|block| match block {
            AssistantContentBlock::ToolCall(call) => Some(call.id.as_str()),
            _ => None,
        })
        .collect();
    for position in dropped {
        let entry = &snapshot[*position];
        let Some(AgentMessage::ToolResult(result)) = &entry.message else {
            continue;
        };
        if surviving_calls.contains(result.tool_call_id.as_str()) {
            return Err(format!(
                "context_edit drops tool result {} whose call {} remains in the path",
                entry.id.as_deref().unwrap_or("<inserted>"),
                result.tool_call_id
            ));
        }
    }
    Ok(())
}

fn inserted_entry(
    role: ContextRole,
    text: &str,
    anchor: Option<usize>,
    snapshot: &[HistoryEntry],
    fallback_turn: usize,
    timestamp: u64,
) -> HistoryEntry {
    let neighbour = anchor.and_then(|position| snapshot.get(position));
    let turn = neighbour.map_or(fallback_turn, |entry| entry.turn);
    let seq = neighbour.map_or(usize::MAX, |entry| entry.seq);
    if role == ContextRole::Notes {
        return HistoryEntry {
            id: None,
            source: None,
            role,
            tokens: text_tokens(text),
            text: text.to_string(),
            message: None,
            turn,
            seq,
        };
    }
    let message = AgentMessage::User(UserMessage {
        content: UserContent::Text(text.to_string()),
        timestamp,
        rest: pa_types::JsonMap::new(),
    });
    HistoryEntry {
        id: None,
        source: None,
        role,
        tokens: estimate_tokens(&message),
        text: text.to_string(),
        message: Some(message),
        turn,
        seq,
    }
}

fn normalize(position: i64, len: usize) -> Option<usize> {
    let len = i64::try_from(len).ok()?;
    let index = if position < 0 {
        len + position
    } else {
        position
    };
    (0..len).contains(&index).then_some(index as usize)
}

/// A half-open range's end bound may be `len` itself (Python's `a[0:len]`).
fn normalize_bound(position: i64, len: usize) -> Option<usize> {
    let len_i64 = i64::try_from(len).ok()?;
    let index = if position < 0 {
        len_i64 + position
    } else {
        position
    };
    (0..=len_i64).contains(&index).then_some(index as usize)
}

fn text_tokens(text: &str) -> u64 {
    (text.chars().count() as u64).div_ceil(4)
}

fn role_of(message: &AgentMessage) -> ContextRole {
    match message {
        AgentMessage::User(_)
        | AgentMessage::BashExecution(_)
        | AgentMessage::Custom(_)
        | AgentMessage::BranchSummary(_)
        | AgentMessage::CompactionSummary(_) => ContextRole::User,
        AgentMessage::Assistant(_) => ContextRole::Assistant,
        AgentMessage::ToolResult(_) => ContextRole::Tool,
    }
}

fn message_text(message: &AgentMessage) -> String {
    match message {
        AgentMessage::User(user) => user.content.text(),
        AgentMessage::Custom(custom) => custom.content.text(),
        AgentMessage::ToolResult(result) => content_text(&result.content),
        AgentMessage::BashExecution(bash) => format!("$ {}", bash.command),
        AgentMessage::BranchSummary(summary) => summary.summary.clone(),
        AgentMessage::CompactionSummary(summary) => summary.summary.clone(),
        AgentMessage::Assistant(assistant) => {
            let mut parts = Vec::new();
            for block in &assistant.content {
                match block {
                    AssistantContentBlock::Text(text) => parts.push(text.text.clone()),
                    AssistantContentBlock::Thinking(thinking) => {
                        parts.push(thinking.thinking.clone());
                    }
                    AssistantContentBlock::ToolCall(call) => {
                        parts.push(format!("[tool call: {}]", call.name));
                    }
                }
            }
            parts.join("\n")
        }
    }
}

fn content_text(content: &[pa_types::ai::UserContentBlock]) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            pa_types::ai::UserContentBlock::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn replaced_message(
    role: ContextRole,
    text: &str,
    original: Option<&AgentMessage>,
) -> Option<AgentMessage> {
    match (role, original) {
        (ContextRole::Tool, Some(AgentMessage::ToolResult(result))) => {
            Some(AgentMessage::ToolResult(ToolResultMessage {
                content: vec![text_block(text)],
                ..result.clone()
            }))
        }
        (ContextRole::Assistant, Some(AgentMessage::Assistant(assistant))) => {
            let mut assistant: AssistantMessage = assistant.clone();
            assistant.content = vec![AssistantContentBlock::Text(TextContent {
                text: text.to_string(),
                text_signature: None,
                rest: pa_types::JsonMap::new(),
            })];
            Some(AgentMessage::Assistant(assistant))
        }
        (ContextRole::Notes, _) => None,
        _ => Some(AgentMessage::User(UserMessage {
            content: UserContent::Text(text.to_string()),
            timestamp: original.map_or(0, |message| match message {
                AgentMessage::User(user) => user.timestamp,
                AgentMessage::Assistant(assistant) => assistant.timestamp,
                AgentMessage::ToolResult(result) => result.timestamp,
                AgentMessage::BashExecution(bash) => bash.timestamp,
                AgentMessage::Custom(custom) => custom.timestamp,
                AgentMessage::BranchSummary(summary) => summary.timestamp,
                AgentMessage::CompactionSummary(summary) => summary.timestamp,
            }),
            rest: pa_types::JsonMap::new(),
        })),
    }
}

fn text_block(text: &str) -> pa_types::ai::UserContentBlock {
    pa_types::ai::UserContentBlock::Text(TextContent {
        text: text.to_string(),
        text_signature: None,
        rest: pa_types::JsonMap::new(),
    })
}

fn message_from_entry(entry: &FileEntry) -> Option<AgentMessage> {
    match entry {
        FileEntry::Message { message, .. } => Some(message.clone()),
        FileEntry::CustomMessage { payload, .. } => {
            Some(AgentMessage::Custom(create_custom_message(payload, entry)))
        }
        FileEntry::BranchSummary { payload, .. } if !payload.summary.is_empty() => {
            Some(AgentMessage::BranchSummary(BranchSummaryMessage {
                rest: pa_types::JsonMap::new(),
                summary: payload.summary.clone(),
                from_id: payload.from_id.clone(),
                timestamp: timestamp_to_millis(entry.timestamp()),
            }))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{build_session_context, parse_session_entries};
    use pa_types::session::{ContextRole, ContextSelector, ContextTarget};

    fn header() -> String {
        r#"{"type":"session","id":"s","timestamp":"2024-01-01T00:00:00.000Z","cwd":"/w","version":3}"#.to_string()
    }

    fn user_row(id: &str, parent: &str, text: &str) -> String {
        serde_json::json!({
            "type": "message",
            "id": id,
            "parentId": if parent.is_empty() { serde_json::Value::Null } else { serde_json::json!(parent) },
            "timestamp": "2024-01-01T00:00:01.000Z",
            "message": {"role": "user", "content": text, "timestamp": 0}
        })
        .to_string()
    }

    fn assistant_row(id: &str, parent: &str, content: &serde_json::Value) -> String {
        serde_json::json!({
            "type": "message",
            "id": id,
            "parentId": parent,
            "timestamp": "2024-01-01T00:00:02.000Z",
            "message": {
                "role": "assistant",
                "content": content,
                "api": "test",
                "provider": "faux",
                "model": "faux-1",
                "usage": {"input": 10, "output": 1, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 11,
                    "cost": {"input": 10, "output": 1, "cacheRead": 0, "cacheWrite": 0, "total": 11}},
                "stopReason": "stop",
                "timestamp": 0
            }
        })
        .to_string()
    }

    fn text_block(text: &str) -> serde_json::Value {
        serde_json::json!({"type": "text", "text": text})
    }

    fn edit_row(id: &str, parent: &str, changes: &serde_json::Value) -> String {
        serde_json::json!({
            "type": "context_edit",
            "id": id,
            "parentId": parent,
            "timestamp": "2024-01-01T00:00:03.000Z",
            "by": "agent",
            "changes": changes,
        })
        .to_string()
    }

    fn chain(len: usize) -> (Vec<FileEntry>, Vec<String>) {
        let mut lines = vec![header()];
        let mut ids = Vec::new();
        for index in 0..len {
            let id = format!("u{index}");
            let parent = if index == 0 {
                String::new()
            } else {
                format!("u{}", index - 1)
            };
            lines.push(user_row(&id, &parent, &format!("m{index}")));
            ids.push(id);
        }
        (parse_session_entries(&lines.join("\n")), ids)
    }

    fn edit(changes: Vec<ContextChange>) -> ContextEditEntry {
        ContextEditEntry {
            by: "agent".to_string(),
            changes,
        }
    }

    fn point(position: i64) -> ContextTarget {
        ContextTarget::Point(ContextSelector::Position(position))
    }

    fn range(start: i64, end: i64) -> ContextTarget {
        ContextTarget::Range(ContextRange {
            start: ContextRangeEnd::Position(start),
            end: ContextRangeEnd::Position(end),
        })
    }

    fn texts(messages: &[AgentMessage]) -> Vec<String> {
        messages.iter().map(message_text).collect()
    }

    #[test]
    fn get_last_region_and_id_address_the_same_view() {
        let (entries, ids) = chain(50);
        let walk = walk_history(&entries, None);
        assert_eq!(walk.entries.len(), 50);

        let last = walk.resolve(&point(-1)).unwrap();
        assert_eq!(last, vec![49]);
        assert_eq!(walk.entries[last[0]].text, "m49");

        let region = walk.resolve(&range(12, 41)).unwrap();
        assert_eq!(region.first().copied(), Some(12));
        assert_eq!(region.last().copied(), Some(40));
        assert_eq!(region.len(), 29);

        let by_id = walk.entry_by_id(&ids[7]).unwrap();
        assert_eq!(by_id.text, "m7");
        assert_eq!(by_id.id.as_deref(), Some("u7"));
    }

    #[test]
    fn drop_removes_the_region_from_the_request_and_keeps_the_record() {
        let (entries, ids) = chain(20);
        let mut walk = walk_history(&entries, None);
        let counts = walk
            .apply(&edit(vec![ContextChange::Drop { at: range(5, 8) }]), 0)
            .unwrap();
        assert_eq!(counts.dropped, 3);
        assert_eq!(walk.entries.len(), 17);
        assert_eq!(walk.hidden.len(), 3);
        assert!(walk.entry_by_id(&ids[5]).is_some());
        assert_eq!(walk.entry_by_id(&ids[5]).unwrap().text, "m5");
        assert!(!texts(&walk.messages()).contains(&"m5".to_string()));
        assert!(texts(&walk.messages()).contains(&"m4".to_string()));
        assert!(texts(&walk.messages()).contains(&"m8".to_string()));
    }

    #[test]
    fn inserted_notes_are_visible_but_never_sent() {
        let (entries, _) = chain(3);
        let mut walk = walk_history(&entries, None);
        let before = texts(&walk.messages());
        let counts = walk
            .apply(
                &edit(vec![ContextChange::Insert {
                    text: "STATE: reading native.rs:417".to_string(),
                    role: ContextRole::Notes,
                    after: ContextAnchor::default(),
                }]),
                0,
            )
            .unwrap();
        assert_eq!(counts.inserted, 1);
        assert_eq!(walk.entries.len(), 4);
        assert_eq!(walk.entries[3].role, ContextRole::Notes);
        assert_eq!(walk.entries[3].text, "STATE: reading native.rs:417");
        assert_eq!(texts(&walk.messages()), before);
    }

    #[test]
    fn two_changes_resolve_against_the_pre_edit_view() {
        let (entries, _) = chain(10);
        let mut walk = walk_history(&entries, None);
        walk.apply(
            &edit(vec![
                ContextChange::Drop { at: range(0, 3) },
                ContextChange::Replace {
                    at: ContextSelector::Position(3),
                    text: "review: 28 frames admitted, 44 affordable".to_string(),
                },
            ]),
            0,
        )
        .unwrap();
        assert_eq!(walk.entries.len(), 7);
        assert_eq!(
            walk.entries[0].text,
            "review: 28 frames admitted, 44 affordable"
        );
        assert_eq!(walk.entries[1].text, "m4");
    }

    #[test]
    fn dropped_tool_result_cannot_strand_its_call() {
        let content =
            serde_json::json!([{"type":"toolCall","id":"call-1","name":"bash","arguments":{}}]);
        let mut lines = vec![
            header(),
            user_row("u0", "", "run it"),
            assistant_row("a0", "u0", &content),
            serde_json::json!({
                "type": "message",
                "id": "t0",
                "parentId": "a0",
                "timestamp": "2024-01-01T00:00:04.000Z",
                "message": {"role": "toolResult", "toolCallId": "call-1", "toolName": "bash",
                    "content": [text_block("done")], "isError": false, "timestamp": 4}
            })
            .to_string(),
        ];
        lines.push(user_row("u1", "t0", "next"));
        let entries = parse_session_entries(&lines.join("\n"));
        let mut walk = walk_history(&entries, None);
        let before: Vec<String> = texts(&walk.messages());
        let error = walk
            .apply(
                &edit(vec![ContextChange::Drop {
                    at: ContextTarget::Point(ContextSelector::EntryId("t0".to_string())),
                }]),
                0,
            )
            .unwrap_err();
        assert!(error.contains("t0"), "{error}");
        assert!(error.contains("call-1"), "{error}");
        assert_eq!(texts(&walk.messages()), before);
        assert_eq!(walk.entries.len(), 4);
    }

    #[test]
    fn replacing_an_assistant_tool_call_is_rejected() {
        let content =
            serde_json::json!([{"type":"toolCall","id":"call-1","name":"bash","arguments":{}}]);
        let lines = [
            header(),
            user_row("u0", "", "run it"),
            assistant_row("a0", "u0", &content),
        ];
        let entries = parse_session_entries(&lines.join("\n"));
        let mut walk = walk_history(&entries, None);
        let error = walk
            .apply(
                &edit(vec![ContextChange::Replace {
                    at: ContextSelector::EntryId("a0".to_string()),
                    text: "rewritten".to_string(),
                }]),
                0,
            )
            .unwrap_err();
        assert!(error.contains("a0"), "{error}");
        assert!(error.contains("tool calls"), "{error}");
    }

    #[test]
    fn unknown_op_and_unused_keys_are_parse_errors() {
        let unknown = serde_json::json!({"op": "explode", "at": 1});
        assert!(ContextChange::parse(&unknown)
            .unwrap_err()
            .contains("unknown op"));
        let unused = serde_json::json!({"op": "drop", "at": 1, "text": "x"});
        assert!(ContextChange::parse(&unused)
            .unwrap_err()
            .contains("does not use key 'text'"));
        let insert_unused =
            serde_json::json!({"op": "insert", "text": "x", "role": "notes", "at": 1});
        assert!(ContextChange::parse(&insert_unused)
            .unwrap_err()
            .contains("does not use key 'at'"));
        let missing = serde_json::json!({"op": "replace", "at": 1});
        assert!(ContextChange::parse(&missing)
            .unwrap_err()
            .contains("needs 'text'"));
    }

    #[test]
    fn an_unknown_op_row_degrades_and_round_trips_verbatim() {
        let line = r#"{"type":"context_edit","id":"e1","parentId":null,"timestamp":"t","by":"agent","changes":[{"op":"explode","at":1}]}"#;
        let parsed: Vec<FileEntry> = parse_session_entries(line);
        assert!(matches!(parsed[0], FileEntry::Unknown { .. }));
        let round_trip = serde_json::to_string(&parsed[0]).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&round_trip).unwrap(),
            serde_json::from_str::<serde_json::Value>(line).unwrap()
        );
    }

    #[test]
    fn a_session_without_edits_sends_as_before() {
        let lines = [
            header(),
            user_row("u0", "", "one"),
            assistant_row("a0", "u0", &serde_json::json!([text_block("two")])),
            user_row("u1", "a0", "three"),
        ];
        let entries = parse_session_entries(&lines.join("\n"));
        let walk = walk_history(&entries, None);
        assert_eq!(texts(&walk.messages()), vec!["one", "two", "three"]);
        assert_eq!(
            serde_json::to_value(build_session_context(&entries, None).messages).unwrap(),
            serde_json::to_value(walk.messages()).unwrap()
        );
    }

    fn compaction_row(id: &str, parent: &str, first_kept: Option<&str>) -> String {
        serde_json::json!({
            "type": "compaction",
            "id": id,
            "parentId": parent,
            "timestamp": "2024-01-01T00:00:05.000Z",
            "summary": "the story so far",
            "firstKeptEntryId": first_kept,
            "tokensBefore": 1234
        })
        .to_string()
    }

    #[test]
    fn compaction_then_edit_substitutes_on_the_same_walk() {
        let lines = [
            header(),
            user_row("u0", "", "old"),
            user_row("u1", "u0", "kept"),
            compaction_row("c0", "u1", Some("u1")),
            edit_row(
                "e0",
                "c0",
                &serde_json::json!([{"op": "insert", "text": "STATE", "role": "notes"}]),
            ),
            user_row("u2", "e0", "after"),
        ];
        let entries = parse_session_entries(&lines.join("\n"));
        let walk = walk_history(&entries, None);
        let sent = texts(&walk.messages());
        assert_eq!(sent.len(), 3, "{sent:?}");
        assert!(sent[0].contains("the story so far"), "{sent:?}");
        assert_eq!(sent[1], "kept");
        assert_eq!(sent[2], "after");
        assert!(walk
            .entries
            .iter()
            .any(|entry| entry.role == ContextRole::Notes));
        assert!(!sent.contains(&"STATE".to_string()));
    }

    #[test]
    fn edit_then_compaction_composes_in_the_other_order() {
        let lines = [
            header(),
            user_row("u0", "", "old"),
            user_row("u1", "u0", "kept"),
            edit_row("e0", "u1", &serde_json::json!([{"op": "drop", "at": 0}])),
            compaction_row("c0", "e0", Some("u1")),
            user_row("u2", "c0", "after"),
        ];
        let entries = parse_session_entries(&lines.join("\n"));
        let walk = walk_history(&entries, None);
        let sent = texts(&walk.messages());
        assert_eq!(sent.len(), 3, "{sent:?}");
        assert!(sent[0].contains("the story so far"), "{sent:?}");
        assert_eq!(sent[1], "kept");
        assert_eq!(sent[2], "after");
    }

    #[test]
    fn auto_compaction_fires_only_above_the_threshold() {
        use crate::session_engine::compaction::{
            estimate_context_tokens, threshold_compaction_due, CompactionSettings,
        };
        let assistant = serde_json::json!([text_block("done")]);
        let mut lines = vec![header(), user_row("u0", "", "work")];
        lines.push(assistant_row("a0", "u0", &assistant));
        let entries = parse_session_entries(&lines.join("\n"));
        // 90k tokens of anchored usage under a 100k window and an 80% threshold.
        let mut entries = entries;
        if let Some(FileEntry::Message {
            message: AgentMessage::Assistant(assistant),
            ..
        }) = entries.iter_mut().find(|entry| entry.id() == Some("a0"))
        {
            assistant.usage.total_tokens = 90_000;
        }
        let full = walk_history(&entries, None).messages();
        let settings = CompactionSettings::default();
        assert!(threshold_compaction_due(&full, 100_000, 1_000, &settings));
        let mut edited = walk_history(&entries, None);
        edited
            .apply(&edit(vec![ContextChange::Drop { at: range(0, 2) }]), 0)
            .unwrap();
        let trimmed = edited.messages();
        assert!(!threshold_compaction_due(
            &trimmed, 100_000, 1_000, &settings
        ));
        assert!(estimate_context_tokens(&trimmed).tokens < estimate_context_tokens(&full).tokens);
    }

    #[test]
    fn selectors_off_the_path_name_the_rule_and_the_target() {
        let (entries, _) = chain(3);
        let mut walk = walk_history(&entries, None);
        let out_of_range = walk
            .apply(&edit(vec![ContextChange::Drop { at: point(9) }]), 0)
            .unwrap_err();
        assert!(
            out_of_range.contains("off the current path"),
            "{out_of_range}"
        );
        assert!(out_of_range.contains('9'), "{out_of_range}");
        let unknown_id = walk
            .apply(
                &edit(vec![ContextChange::Drop {
                    at: ContextTarget::Point(ContextSelector::EntryId("nope".to_string())),
                }]),
                0,
            )
            .unwrap_err();
        assert!(unknown_id.contains("nope"), "{unknown_id}");
        assert!(unknown_id.contains("not in this session"), "{unknown_id}");
    }

    fn assistant_row_with_usage(id: &str, parent: &str, input: u64) -> String {
        serde_json::json!({
            "type": "message",
            "id": id,
            "parentId": parent,
            "timestamp": "2024-01-01T00:00:02.000Z",
            "message": {
                "role": "assistant",
                "content": [text_block("ok")],
                "api": "test",
                "provider": "faux",
                "model": "faux-1",
                "usage": {"input": input, "output": 1, "cacheRead": 0, "cacheWrite": 0,
                    "totalTokens": input + 1,
                    "cost": {"input": 0.0, "output": 0.0, "cacheRead": 0.0, "cacheWrite": 0.0, "total": 0.0}},
                "stopReason": "stop",
                "timestamp": 0
            }
        })
        .to_string()
    }

    fn archived_compaction_row(id: &str, parent: &str) -> String {
        compaction_row_with_archive(id, parent, "standard")
    }

    fn compaction_row_with_archive(id: &str, parent: &str, kind: &str) -> String {
        let representation = if kind == "responses" {
            serde_json::json!({
                "kind": "responses",
                "provider": "openai",
                "endpoint": "responses",
                "model": "m",
                "archive": "body"
            })
        } else {
            serde_json::json!({"kind": "standard"})
        };
        serde_json::json!({
            "type": "compaction",
            "id": id,
            "parentId": parent,
            "timestamp": "2024-01-01T00:00:05.000Z",
            "summary": "the story so far",
            "firstKeptEntryId": parent,
            "tokensBefore": 1234,
            "contextArchive": {
                "version": 1,
                "strategy": "standard",
                "compactor": "c",
                "creatorGeneration": "g",
                "preparedLeaf": parent,
                "coveredEntryIds": ["u0"],
                "representation": representation
            },
        })
        .to_string()
    }

    #[test]
    fn edit_then_a_portable_compaction_retires_the_usage_and_the_drift() {
        let big = "x".repeat(200_000);
        let lines = [
            header(),
            user_row("u0", "", &big),
            assistant_row_with_usage("a0", "u0", 90_000),
            edit_row("e0", "a0", &serde_json::json!([{"op": "drop", "at": 0}])),
            compaction_row("c0", "e0", Some("a0")),
        ];
        let entries = parse_session_entries(&lines.join("\n"));
        let walk = walk_history(&entries, None);
        assert!(
            walk.usage_drift.is_clean(),
            "the compaction replaced the prefix the drop corrected"
        );
        assert!(
            walk.usage_drift.boundary.is_some(),
            "a boundary was recorded"
        );
        let messages = walk.messages();
        let chars: u64 = messages.iter().map(estimate_tokens).sum();
        let figure = crate::session_engine::compaction::context_tokens_with_drift(
            &messages,
            walk.usage_drift,
        );
        assert_eq!(figure, chars, "the summary and its retained turn only");
        assert!(figure < 100, "{figure}");
        assert_eq!(
            crate::session_engine::turn_boundary::context_usage(
                &messages,
                walk.usage_drift,
                Some(200_000)
            )
            .unwrap()
            .tokens,
            Some(chars),
            "the status reader reports the figure index reports"
        );
        assert!(
            !crate::session_engine::compaction::threshold_compaction_due_with_drift(
                &messages,
                walk.usage_drift,
                200_000,
                0,
                &crate::session_engine::compaction::CompactionSettings {
                    threshold_percent: 50.0,
                    enabled: true,
                    reserve_tokens: 0,
                    keep_recent_tokens: 1_000,
                }
            ),
            "a retired 90_000-token anchor does not decide"
        );
    }

    #[test]
    fn a_native_compaction_retires_the_usage_behind_it() {
        let big = "x".repeat(200_000);
        let kept = "k".repeat(4_000);
        let lines = [
            header(),
            user_row("u0", "", &big),
            assistant_row_with_usage("a0", "u0", 90_000),
            user_row("u1", "a0", &kept),
            compaction_row_with_archive("c0", "u1", "responses"),
        ];
        let entries = parse_session_entries(&lines.join("\n"));
        let walk = walk_history(&entries, None);
        assert!(walk.usage_drift.boundary.is_some());
        let messages = walk.messages();
        assert_eq!(messages.len(), 2, "the native summary is not a message");
        let figure = crate::session_engine::compaction::context_tokens_with_drift(
            &messages,
            walk.usage_drift,
        );
        assert_eq!(figure, 1_001, "the retained 4_000-char row only");
        assert_eq!(
            crate::session_engine::turn_boundary::context_usage(
                &messages,
                walk.usage_drift,
                Some(200_000)
            )
            .unwrap()
            .tokens,
            Some(1_001),
            "a pre-compaction anchor does not measure the request"
        );
        assert!(
            !crate::session_engine::compaction::threshold_compaction_due_with_drift(
                &messages,
                walk.usage_drift,
                200_000,
                0,
                &crate::session_engine::compaction::CompactionSettings {
                    threshold_percent: 50.0,
                    enabled: true,
                    reserve_tokens: 0,
                    keep_recent_tokens: 1_000,
                }
            )
        );
    }

    #[test]
    fn an_edit_after_a_compaction_does_not_revive_the_retired_usage() {
        let lines = [
            header(),
            user_row("u0", "", "old"),
            user_row("u1", "u0", "kept"),
            assistant_row_with_usage("a0", "u1", 90_000),
            compaction_row("c0", "a0", Some("u1")),
            edit_row(
                "e0",
                "c0",
                &serde_json::json!([{"op": "replace", "at": "u1", "text": "y".repeat(100_000)}]),
            ),
        ];
        let entries = parse_session_entries(&lines.join("\n"));
        let walk = walk_history(&entries, None);
        let messages = walk.messages();
        let chars: u64 = messages.iter().map(estimate_tokens).sum();
        let figure = crate::session_engine::compaction::context_tokens_with_drift(
            &messages,
            walk.usage_drift,
        );
        assert_eq!(figure, chars, "the view, not the retired anchor");
        assert!(figure < 90_000, "{figure}");
        assert!(
            walk.usage_drift.boundary.is_some(),
            "the edit keeps the compaction boundary"
        );
    }

    #[test]
    fn replacing_the_compaction_row_retires_the_usage_it_carried() {
        let big = "x".repeat(40_000);
        let lines = [
            header(),
            user_row("u0", "", &big),
            assistant_row_with_usage("a0", "u0", 80_000),
            archived_compaction_row("c0", "a0"),
            edit_row(
                "e0",
                "c0",
                &serde_json::json!([{"op": "replace", "at": "c0", "text": "rewritten summary"}]),
            ),
        ];
        let entries = parse_session_entries(&lines.join("\n"));
        let walk = walk_history(&entries, None);
        assert!(
            walk.archive.is_none(),
            "replacing the archive row stops its expansion"
        );
        assert!(walk.usage_drift.anchor_lost);
        let messages = walk.messages();
        let chars: u64 = messages.iter().map(estimate_tokens).sum();
        assert_eq!(
            crate::session_engine::compaction::context_tokens_with_drift(
                &messages,
                walk.usage_drift
            ),
            chars,
            "the summary is replaced, so no observation describes the view"
        );
    }

    #[test]
    fn a_usage_reported_after_a_compaction_is_honoured_and_clears_the_correction() {
        let lines = [
            header(),
            user_row("u0", "", "old"),
            user_row("u1", "u0", "kept"),
            assistant_row_with_usage("a0", "u1", 90_000),
            compaction_row("c0", "a0", Some("u1")),
            edit_row(
                "e0",
                "c0",
                &serde_json::json!([{"op": "insert", "text": "y".repeat(40_000), "role": "user", "after": 1}]),
            ),
            assistant_row_with_usage("a1", "e0", 20_000),
        ];
        let mut entries = parse_session_entries(&lines.join("\n"));
        if let Some(FileEntry::Message {
            message: AgentMessage::Assistant(assistant),
            ..
        }) = entries.iter_mut().find(|entry| entry.id() == Some("a1"))
        {
            assistant.timestamp = 1_704_067_209_000;
        }
        let retired = walk_history(&entries, Some("e0"));
        assert!(retired.usage_drift.boundary.is_some());
        assert_eq!(
            retired.usage_drift.tokens, 10_000,
            "the insert above the retired anchor corrects nothing yet"
        );
        let retired_figure = crate::session_engine::compaction::context_tokens_with_drift(
            &retired.messages(),
            retired.usage_drift,
        );
        let chars: u64 = retired.messages().iter().map(estimate_tokens).sum();
        assert_eq!(retired_figure, chars, "the anchor is behind the boundary");

        let walk = walk_history(&entries, None);
        let messages = walk.messages();
        let fresh = crate::session_engine::compaction::context_tokens_with_drift(
            &messages,
            walk.usage_drift,
        );
        assert_eq!(
            walk.usage_drift.tokens, 0,
            "the fresh anchor needs no correction"
        );
        assert!(
            walk.usage_drift.boundary.is_some(),
            "the boundary outlives the correction"
        );
        assert_eq!(
            fresh, 20_001,
            "the observation after the boundary is used, with nothing trailing it"
        );
        assert_ne!(fresh, chars);
    }

    #[test]
    fn dropping_the_compaction_row_clears_its_archive() {
        let lines = [
            header(),
            user_row("u0", "", "old"),
            user_row("u1", "u0", "kept"),
            archived_compaction_row("c0", "u1"),
            user_row("u2", "c0", "after"),
            edit_row("e0", "u2", &serde_json::json!([{"op": "drop", "at": 0}])),
        ];
        let entries = parse_session_entries(&lines.join("\n"));
        let walk = walk_history(&entries, None);
        assert!(walk.archive.is_none(), "the archive's row left the view");
        assert_eq!(
            walk.entries
                .iter()
                .map(|entry| entry.text.as_str())
                .collect::<Vec<_>>(),
            vec!["kept", "after"],
            "the dropped summary is hidden"
        );
        assert!(walk
            .hidden
            .iter()
            .any(|entry| entry.source.is_some() && entry.role == ContextRole::User));
    }

    #[test]
    fn a_native_archive_is_an_addressable_view_entry() {
        let lines = [
            header(),
            user_row("u0", "", "old"),
            user_row("u1", "u0", "kept"),
            compaction_row_with_archive("c0", "u1", "responses"),
            user_row("u2", "c0", "after"),
            edit_row("e1", "u2", &serde_json::json!([{"op": "drop", "at": "c0"}])),
        ];
        let entries = parse_session_entries(&lines.join("\n"));
        let walk = walk_history(&entries, None);
        assert!(
            walk.archive.is_none(),
            "dropping the archive's view entry stops its expansion"
        );
        assert_eq!(
            walk.entries
                .iter()
                .map(|entry| entry.text.as_str())
                .collect::<Vec<_>>(),
            vec!["kept", "after"]
        );
        assert_eq!(walk.messages().len(), 2, "a view-only entry is never sent");
    }

    #[test]
    fn an_untouched_archive_survives_the_walk() {
        let lines = [
            header(),
            user_row("u0", "", "old"),
            user_row("u1", "u0", "kept"),
            archived_compaction_row("c0", "u1"),
            user_row("u2", "c0", "after"),
        ];
        let entries = parse_session_entries(&lines.join("\n"));
        let walk = walk_history(&entries, None);
        assert!(walk.archive.is_some());
    }

    #[test]
    fn dropping_a_row_before_the_usage_anchor_moves_the_estimate() {
        let big = "x".repeat(40_000);
        let lines = [
            header(),
            user_row("u0", "", &big),
            user_row("u1", "u0", "kept"),
            assistant_row_with_usage("a0", "u1", 20_000),
            user_row("u2", "a0", "after"),
        ];
        let entries = parse_session_entries(&lines.join("\n"));
        let mut walk = walk_history(&entries, None);
        let before = crate::session_engine::compaction::context_tokens_with_drift(
            &walk.messages(),
            walk.usage_drift,
        );
        walk.apply(&edit(vec![ContextChange::Drop { at: point(0) }]), 0)
            .unwrap();
        assert_eq!(walk.usage_drift.tokens, -10_000, "40_000 chars / 4");
        let after = crate::session_engine::compaction::context_tokens_with_drift(
            &walk.messages(),
            walk.usage_drift,
        );
        assert_eq!(after, before - 10_000, "{before} -> {after}");
    }

    #[test]
    fn an_edit_that_removes_the_usage_anchor_falls_back_to_the_char_count() {
        let big = "x".repeat(40_000);
        let lines = [
            header(),
            user_row("u0", "", &big),
            user_row("u1", "u0", "kept"),
            assistant_row_with_usage("a0", "u1", 1_000),
            user_row("u2", "a0", "after"),
        ];
        let entries = parse_session_entries(&lines.join("\n"));
        let mut walk = walk_history(&entries, None);
        walk.apply(&edit(vec![ContextChange::Drop { at: point(2) }]), 0)
            .unwrap();
        assert!(walk.usage_drift.anchor_lost);
        let estimate = crate::session_engine::compaction::context_tokens_with_drift(
            &walk.messages(),
            walk.usage_drift,
        );
        let chars: u64 = walk.messages().iter().map(estimate_tokens).sum();
        assert_eq!(estimate, chars, "no anchor: the whole view is estimated");
        assert!(estimate > 10_000, "{estimate}");
    }

    #[test]
    fn deleting_below_the_threshold_stops_compaction_from_firing() {
        let big = "x".repeat(400_000);
        let lines = [
            header(),
            user_row("u0", "", &big),
            user_row("u1", "u0", "kept"),
            assistant_row_with_usage("a0", "u1", 90_000),
            edit_row("e0", "a0", &serde_json::json!([{"op": "drop", "at": 0}])),
        ];
        let entries = parse_session_entries(&lines.join("\n"));
        let walk = walk_history(&entries, None);
        let settings = crate::session_engine::compaction::CompactionSettings {
            threshold_percent: 90.0,
            enabled: true,
            reserve_tokens: 0,
            keep_recent_tokens: 1_000,
        };
        let messages = walk.messages();
        assert!(
            crate::session_engine::compaction::threshold_compaction_due(
                &messages, 100_000, 0, &settings
            ),
            "the uncorrected anchor still reports the dropped row"
        );
        assert!(
            !crate::session_engine::compaction::threshold_compaction_due_with_drift(
                &messages,
                walk.usage_drift,
                100_000,
                0,
                &settings
            ),
            "the edited view fits"
        );
    }

    #[test]
    fn drop_and_replace_naming_one_entry_is_rejected() {
        let (entries, _ids) = chain(4);
        let mut walk = walk_history(&entries, None);
        let error = walk
            .apply(
                &edit(vec![
                    ContextChange::Drop { at: range(0, 3) },
                    ContextChange::Replace {
                        at: ContextSelector::Position(1),
                        text: "rewritten".to_string(),
                    },
                ]),
                0,
            )
            .unwrap_err();
        assert!(error.contains("drop and replace"), "{error}");
        assert_eq!(walk.entries.len(), 4, "a rejected edit mutates nothing");
    }

    #[test]
    fn replacing_a_row_before_the_usage_anchor_counts_the_replacement() {
        let lines = [
            header(),
            user_row("u0", "", "x"),
            user_row("u1", "u0", "kept"),
            assistant_row_with_usage("a0", "u1", 2_000),
            user_row("u2", "a0", "after"),
        ];
        let entries = parse_session_entries(&lines.join(
            "
",
        ));
        let mut walk = walk_history(&entries, None);
        let old_tokens = walk.entries[0].tokens;
        let before = crate::session_engine::compaction::context_tokens_with_drift(
            &walk.messages(),
            walk.usage_drift,
        );
        walk.apply(
            &edit(vec![ContextChange::Replace {
                at: ContextSelector::Position(0),
                text: "y".repeat(200_000),
            }]),
            0,
        )
        .unwrap();
        let after = crate::session_engine::compaction::context_tokens_with_drift(
            &walk.messages(),
            walk.usage_drift,
        );
        assert_eq!(
            walk.usage_drift.tokens,
            50_000 - i64::try_from(old_tokens).unwrap(),
            "the replacement's own estimate less the row it replaced"
        );
        assert_eq!(after, before - old_tokens + 50_000, "{before} -> {after}");
        assert!(after > 49_000, "{after}");
    }

    #[test]
    fn a_view_without_an_anchor_compacts_on_its_own_char_count() {
        let big = "x".repeat(500_000);
        let lines = [
            header(),
            user_row("u0", "", &big),
            assistant_row_with_usage("a0", "u0", 1_000),
            edit_row("e0", "a0", &serde_json::json!([{"op": "drop", "at": 1}])),
        ];
        let entries = parse_session_entries(&lines.join(
            "
",
        ));
        let walk = walk_history(&entries, None);
        let settings = crate::session_engine::compaction::CompactionSettings {
            threshold_percent: 95.0,
            enabled: true,
            reserve_tokens: 0,
            keep_recent_tokens: 1_000,
        };
        let messages = walk.messages();
        assert!(walk.usage_drift.anchor_lost);
        let estimate = crate::session_engine::compaction::context_tokens_with_drift(
            &messages,
            walk.usage_drift,
        );
        assert_eq!(estimate, 125_000, "500_000 chars / 4");
        assert!(
            crate::session_engine::compaction::threshold_compaction_due_with_drift(
                &messages,
                walk.usage_drift,
                100_000,
                0,
                &settings
            ),
            "a 125_000-token view over a 100_000 window is due"
        );
        assert!(
            !crate::session_engine::compaction::threshold_compaction_due_with_drift(
                &messages,
                walk.usage_drift,
                200_000,
                0,
                &settings
            ),
            "and a view that fits is not"
        );
    }

    #[test]
    fn dropping_the_compaction_row_leaves_no_anchor_for_the_prefix() {
        let lines = [
            header(),
            user_row("u0", "", "old"),
            compaction_row("c0", "u0", Some("u1")),
            user_row("u1", "c0", "kept"),
            assistant_row_with_usage("a0", "u1", 90_000),
            edit_row("e0", "a0", &serde_json::json!([{"op": "drop", "at": 0}])),
        ];
        let entries = parse_session_entries(&lines.join(
            "
",
        ));
        let walk = walk_history(&entries, None);
        assert!(
            walk.usage_drift.anchor_lost,
            "the dropped summary was part of what the anchor counted"
        );
        let estimate = crate::session_engine::compaction::context_tokens_with_drift(
            &walk.messages(),
            walk.usage_drift,
        );
        let chars: u64 = walk.messages().iter().map(estimate_tokens).sum();
        assert_eq!(estimate, chars, "no anchor: the whole view is estimated");
        assert!(estimate < 100, "{estimate}");
    }

    #[test]
    fn replace_then_drop_naming_one_entry_is_rejected_too() {
        let (entries, _ids) = chain(4);
        let mut walk = walk_history(&entries, None);
        let error = walk
            .apply(
                &edit(vec![
                    ContextChange::Replace {
                        at: ContextSelector::Position(1),
                        text: "rewritten".to_string(),
                    },
                    ContextChange::Drop { at: point(1) },
                ]),
                0,
            )
            .unwrap_err();
        assert!(error.contains("drop and replace"), "{error}");
        assert_eq!(walk.entries.len(), 4, "a rejected edit mutates nothing");
        assert_eq!(walk.entries[1].text, "m1");
    }

    #[test]
    fn a_fresh_usage_anchor_resets_the_edit_correction() {
        let big = "x".repeat(200_000);
        let lines = [
            header(),
            user_row("u0", "", &big),
            user_row("u1", "u0", "kept"),
            assistant_row_with_usage("a0", "u1", 90_000),
            edit_row("e0", "a0", &serde_json::json!([{"op": "drop", "at": 2}])),
            assistant_row_with_usage("a1", "e0", 40_000),
            user_row("u2", "a1", "after"),
            edit_row("e1", "u2", &serde_json::json!([{"op": "drop", "at": "u1"}])),
        ];
        let lost = walk_history(&parse_session_entries(&lines[..5].join("\n")), None);
        assert!(
            lost.usage_drift.anchor_lost,
            "dropping a0 leaves no observation that covers the view"
        );
        let lost_figure = crate::session_engine::compaction::context_tokens_with_drift(
            &lost.messages(),
            lost.usage_drift,
        );
        let lost_chars: u64 = lost.messages().iter().map(estimate_tokens).sum();
        assert_eq!(lost_figure, lost_chars);
        assert!(lost_figure > 50_000, "{lost_figure}");

        let walk = walk_history(&parse_session_entries(&lines.join("\n")), None);
        let settings = crate::session_engine::compaction::CompactionSettings {
            threshold_percent: 1.0,
            enabled: true,
            reserve_tokens: 0,
            keep_recent_tokens: 1_000,
        };
        let after_second_edit = crate::session_engine::compaction::context_tokens_with_drift(
            &walk.messages(),
            walk.usage_drift,
        );
        let dropped = walk
            .hidden
            .iter()
            .find(|entry| entry.id.as_deref() == Some("u1"))
            .unwrap();
        assert_eq!(
            walk.usage_drift.tokens,
            -i64::try_from(dropped.tokens).unwrap(),
            "the second edit drops one row above the 40_000 anchor"
        );
        let trailing = estimate_tokens(walk.messages().last().unwrap());
        assert_eq!(
            after_second_edit,
            40_001 + trailing - dropped.tokens,
            "the anchor reports the edited view, without the 50_000-token drop"
        );
        assert!(
            !walk.usage_drift.anchor_lost,
            "the first edit's lost anchor is forgotten once a new one reports"
        );
        assert!(
            crate::session_engine::compaction::threshold_compaction_due_with_drift(
                &walk.messages(),
                walk.usage_drift,
                200_000,
                0,
                &settings,
            ),
            "a fresh 40_000-token observation is not the 0 the stale drift claimed"
        );
    }

    #[test]
    fn a_note_before_the_anchor_does_not_move_the_estimate() {
        let big = "x".repeat(200_000);
        let note = "y".repeat(200_000);
        let lines = [
            header(),
            user_row("u0", "", &big),
            assistant_row_with_usage("a0", "u0", 90_000),
            edit_row(
                "e0",
                "a0",
                &serde_json::json!([{"op": "insert", "text": note, "role": "notes", "after": 0}]),
            ),
            edit_row("e1", "e0", &serde_json::json!([{"op": "drop", "at": 1}])),
            edit_row(
                "e2",
                "e1",
                &serde_json::json!([{"op": "insert", "text": note, "role": "notes", "after": 0}]),
            ),
            edit_row(
                "e3",
                "e2",
                &serde_json::json!([{"op": "replace", "at": 1, "text": "z".repeat(100_000)}]),
            ),
        ];
        let entries = parse_session_entries(&lines.join("\n"));
        let walk = walk_history(&entries, None);
        assert_eq!(walk.skipped_edits, 0);
        assert!(
            walk.usage_drift.is_clean(),
            "a note has no message, so no edit of one changes what the anchor counted"
        );
        let estimate = crate::session_engine::compaction::context_tokens_with_drift(
            &walk.messages(),
            walk.usage_drift,
        );
        assert_eq!(
            estimate,
            crate::session_engine::compaction::estimate_context_tokens(&walk.messages()).tokens,
            "no correction: the anchor plus whatever trails it"
        );
        assert!((90_000..91_000).contains(&estimate), "{estimate}");
    }

    #[test]
    fn a_range_end_an_earlier_edit_dropped_uses_the_off_path_rule() {
        let (entries, _ids) = chain(5);
        let mut walk = walk_history(&entries, None);
        walk.apply(&edit(vec![ContextChange::Drop { at: point(1) }]), 0)
            .unwrap();
        let error = walk
            .apply(
                &edit(vec![ContextChange::Drop {
                    at: ContextTarget::Range(ContextRange {
                        start: ContextRangeEnd::PathBound,
                        end: ContextRangeEnd::EntryId("u1".to_string()),
                    }),
                }]),
                0,
            )
            .unwrap_err();
        assert!(error.contains("no longer live"), "{error}");
        assert!(error.contains("u1"), "{error}");
    }

    #[test]
    fn an_insert_anchored_to_a_dropped_row_still_resolves() {
        let (entries, _ids) = chain(6);
        let mut walk = walk_history(&entries, None);
        walk.apply(
            &edit(vec![
                ContextChange::Drop { at: point(2) },
                ContextChange::Insert {
                    text: "STATE".to_string(),
                    role: ContextRole::Notes,
                    after: ContextAnchor::Position(2),
                },
            ]),
            0,
        )
        .unwrap();
        let ids: Vec<Option<&str>> = walk
            .entries
            .iter()
            .map(|entry| entry.id.as_deref())
            .collect();
        assert_eq!(
            ids,
            vec![
                Some("u0"),
                Some("u1"),
                None,
                Some("u3"),
                Some("u4"),
                Some("u5")
            ]
        );
    }

    #[test]
    fn an_edit_that_no_longer_resolves_is_counted_rather_than_silently_skipped() {
        let lines = [
            header(),
            user_row("u0", "", "one"),
            user_row("u1", "u0", "two"),
            user_row("u2", "u1", "three"),
            edit_row(
                "e0",
                "u2",
                &serde_json::json!([{"op": "drop", "at": [0, 2]}]),
            ),
            edit_row("e1", "e0", &serde_json::json!([{"op": "drop", "at": 3}])),
        ];
        let entries = parse_session_entries(&lines.join("\n"));
        let walk = walk_history(&entries, None);
        assert_eq!(walk.skipped_edits, 1, "e1 addresses a row e0 removed");
        assert_eq!(
            walk.entries
                .iter()
                .map(|entry| entry.text.as_str())
                .collect::<Vec<_>>(),
            vec!["three"]
        );
    }

    #[test]
    fn an_inserted_note_can_be_dropped_by_position_and_survives_compaction() {
        let lines = [
            header(),
            user_row("u0", "", "one"),
            edit_row(
                "e0",
                "u0",
                &serde_json::json!([{"op": "insert", "text": "STATE", "after": 0}]),
            ),
            compaction_row("c0", "e0", Some("u0")),
        ];
        let entries = parse_session_entries(&lines.join("\n"));
        let walk = walk_history(&entries, None);
        let notes: Vec<&HistoryEntry> = walk
            .entries
            .iter()
            .filter(|entry| entry.role == ContextRole::Notes)
            .collect();
        assert_eq!(notes.len(), 1, "a note is not a covered record");
        assert!(notes[0].id.is_none());
        assert_eq!(walk.messages().len(), 2, "summary plus the kept row");
    }
}
