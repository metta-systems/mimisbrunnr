use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::PathBuf,
};

use {clap::Parser, eframe::egui};

use mimisbrunnr::{
    engine::DiskEngine,
    index::ForwardEntry,
    meta::RECORD_SIZE,
    ontology::{TagDefinition, TagSemantics},
    pool::PoolConfig,
    storage::ZoneType,
    types::{Assertion, CompressionState, ObjectId, ObjectState, TagId, TagOrigin, Value},
};

// ── CLI ──────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "analyze", about = "Visual pool analyzer for Mímisbrunnr")]
struct Cli {
    /// Path to pool.toml
    pool: PathBuf,
}

// ── Snapshot ─────────────────────────────────────────────────────────

/// Pre-extracted pool data for the UI (no borrow lifetime issues).
struct PoolSnapshot {
    config: PoolConfig,
    node_id: u64,
    tags: Vec<TagInfo>,
    tag_by_id: HashMap<u32, usize>,
    objects: Vec<ObjInfo>,
    implications: Vec<(u32, u32)>,
    contexts: Vec<ContextInfo>,
    placement_rules: Vec<String>,
    default_compression: String,
    blob_count: usize,
    total_blob_bytes: u64,
    /// Path to the primary disk file for raw reads.
    disk_path: PathBuf,
    /// Metadata zone offset on disk.
    metadata_zone_offset: u64,
    /// Index zone offset on disk.
    index_zone_offset: u64,
    /// Blob zone offset on disk.
    blob_zone_offset: u64,
}

#[derive(Clone)]
struct TagInfo {
    id: TagId,
    name: String,
    semantics: String,
    object_count: u32,
    implies: Vec<u32>,
    implied_by: Vec<u32>,
}

#[derive(Clone)]
struct ObjInfo {
    oid: ObjectId,
    id_raw: u64,
    local: u64,
    state: ObjectState,
    compression: CompressionState,
    blob_length: u64,
    stored_size: u64,
    created_ns: i64,
    modified_ns: i64,
    generation: u32,
    content_hash: [u8; 32],
    tag_count: u16,
    attr_count: u16,
    direct_tags: Vec<u32>,
    materialized_tags: Vec<u32>,
    attrs: Vec<(String, String)>,
    paths: Vec<(String, String)>,
    has_blob: bool,
    blob_size: Option<usize>,
    /// First 4 KiB of blob data (from in-memory store) for hex preview.
    blob_preview: Vec<u8>,
    /// Physical disk offset of the 128-byte ObjectRecord.
    record_disk_offset: u64,
    /// Physical disk offset of blob data (from ObjectRecord).
    blob_disk_offset: u64,
}

#[derive(Clone)]
struct ContextInfo {
    name: String,
    entry_count: usize,
}

fn extract_snapshot(de: &DiskEngine) -> PoolSnapshot {
    let engine = de.engine();
    let layout = &de.superblock().layout;

    // Tags
    let all_tag_ids = engine.dag.all_tags();
    let mut tags: Vec<TagInfo> = Vec::new();
    let mut tag_by_id: HashMap<u32, usize> = HashMap::new();

    for tid in &all_tag_ids {
        let def: Option<&TagDefinition> = engine.dag.get(*tid);
        let bitmap = engine.tag_index.bitmap(*tid);
        let object_count = bitmap.map_or(0, |b| b.len() as u32);

        let implies: Vec<u32> = engine
            .dag
            .direct_implies(*tid)
            .iter()
            .map(|t| t.raw())
            .collect();

        let info = TagInfo {
            id: *tid,
            name: def.map_or_else(|| format!("tag_{}", tid.raw()), |d| d.name.clone()),
            semantics: def.map_or_else(
                || "unknown".into(),
                |d| match &d.semantics {
                    TagSemantics::Label => "label".into(),
                    TagSemantics::Attribute { value_type } => format!("attr({value_type:?})"),
                    TagSemantics::Grouping => "grouping".into(),
                    TagSemantics::OrderedCollection { .. } => "ordered".into(),
                    TagSemantics::Hierarchical => "hierarchical".into(),
                },
            ),
            object_count,
            implies,
            implied_by: Vec::new(),
        };
        tag_by_id.insert(tid.raw(), tags.len());
        tags.push(info);
    }

    // Build implied_by reverse edges
    let implications: Vec<(u32, u32)> = {
        let mut imps = Vec::new();
        // Collect edges first to avoid borrow conflict
        let edges: Vec<(u32, u32)> = tags
            .iter()
            .flat_map(|t| t.implies.iter().map(move |&target| (t.id.raw(), target)))
            .collect();
        for (from, to) in &edges {
            imps.push((*from, *to));
            if let Some(&idx) = tag_by_id.get(to) {
                tags[idx].implied_by.push(*from);
            }
        }
        imps
    };

    // Objects
    let mut objects: Vec<ObjInfo> = Vec::new();
    for rec in engine.object_table.iter() {
        let node = rec.id >> 48;
        let local = rec.id & 0x0000_FFFF_FFFF_FFFF;
        let oid = ObjectId::new(node, local);

        let entries: &[ForwardEntry] = engine.forward_index.get(oid);
        let mut direct_tags = Vec::new();
        let mut materialized_tags = Vec::new();
        let mut attrs = Vec::new();

        for entry in entries {
            match (&entry.assertion, &entry.origin) {
                (Assertion::Tag(tid), TagOrigin::Direct) => direct_tags.push(tid.raw()),
                (Assertion::Tag(tid), TagOrigin::Materialized) => materialized_tags.push(tid.raw()),
                (Assertion::Attr { key, value }, _) => {
                    let key_name = engine
                        .dag
                        .get(*key)
                        .map_or_else(|| format!("attr_{}", key.raw()), |d| d.name.clone());
                    attrs.push((key_name, format_value(value)));
                }
                _ => {}
            }
        }

        let record_disk_offset = layout
            .logical_to_physical(ZoneType::Metadata, local * RECORD_SIZE as u64)
            .unwrap_or(0);

        // Read transformed blob data from blob zone
        let blob_data = de.read_blob(oid).ok().filter(|b| !b.is_empty());

        // Paths
        let ctx_entries = de.context_mgr.contexts_for_object(oid);
        let paths: Vec<(String, String)> = ctx_entries
            .iter()
            .map(|(ctx, entry)| {
                let ctx_name = ctx.map_or("(unscoped)".to_string(), |s| s.to_string());
                (ctx_name, entry.path.clone())
            })
            .collect();

        objects.push(ObjInfo {
            oid,
            id_raw: rec.id,
            local,
            state: rec.state(),
            compression: rec.compression(),
            blob_length: rec.blob_length,
            stored_size: rec.stored_size,
            created_ns: rec.created_ns,
            modified_ns: rec.modified_ns,
            generation: rec.generation,
            content_hash: rec.content_hash,
            tag_count: rec.tag_count,
            attr_count: rec.attr_count,
            direct_tags,
            materialized_tags,
            attrs,
            paths,
            has_blob: blob_data.is_some(),
            blob_size: blob_data.as_ref().map(|b| b.len()),
            blob_preview: blob_data
                .map(|b| b[..b.len().min(4096)].to_vec())
                .unwrap_or_default(),
            record_disk_offset,
            blob_disk_offset: rec.blob_offset,
        });
    }

    // Contexts
    let mut contexts: Vec<ContextInfo> = Vec::new();
    for ctx_name in de.context_mgr.list_contexts() {
        if let Ok(proj) = de.context_mgr.get_context(ctx_name) {
            contexts.push(ContextInfo {
                name: ctx_name.to_string(),
                entry_count: proj.len(),
            });
        }
    }
    let unscoped_count = de.context_mgr.unscoped().len();
    if unscoped_count > 0 {
        contexts.push(ContextInfo {
            name: "(unscoped)".to_string(),
            entry_count: unscoped_count,
        });
    }

    // Placement rules
    let placement_rules: Vec<String> = engine.rules().iter().map(format_rule).collect();

    // Blob stats — compute from object records
    let blob_count = objects.iter().filter(|o| o.has_blob).count();
    let total_blob_bytes: u64 = objects.iter().map(|o| o.stored_size).sum();

    // Get disk path from config
    let disk_path = de
        .config()
        .primary_disk()
        .map(|d| PathBuf::from(&d.path))
        .unwrap_or_default();

    PoolSnapshot {
        config: PoolConfig::load(&PathBuf::from("")).unwrap_or_else(|_| PoolConfig::new(0)),
        node_id: engine.node_id(),
        tags,
        tag_by_id,
        objects,
        implications,
        contexts,
        placement_rules,
        default_compression: format!("{:?}", engine.rules()),
        blob_count,
        total_blob_bytes,
        disk_path,
        metadata_zone_offset: layout.metadata_zone_offset(),
        index_zone_offset: layout.index_zone_offset(),
        blob_zone_offset: layout.blob_zone_offset(),
    }
}

fn format_value(v: &Value) -> String {
    match v {
        Value::Text(s) => format!("\"{s}\""),
        Value::Int(n) => n.to_string(),
        Value::Float(f) => format!("{f:.2}"),
        Value::Timestamp(t) => format!("ts:{t}"),
        Value::Blob(b) => format!("<{} bytes>", b.len()),
    }
}

fn format_rule(rule: &mimisbrunnr::pool::PlacementRule) -> String {
    use mimisbrunnr::pool::PlacementRule;
    match rule {
        PlacementRule::Compress { query, algo } => {
            format!("Compress({query:?} → {algo:?})")
        }
        PlacementRule::Pin { query, tier } => {
            format!("Pin({query:?} → {})", tier.name())
        }
        PlacementRule::Prefer {
            query,
            tier,
            priority,
        } => {
            format!("Prefer({query:?} → {} p={priority})", tier.name())
        }
        PlacementRule::Replicate {
            query,
            min_replicas,
            across_disks,
        } => {
            format!("Replicate({query:?} ×{min_replicas} across={across_disks})")
        }
        PlacementRule::Colocate { query } => format!("Colocate({query:?})"),
        PlacementRule::AutoTier {
            hot_threshold_days,
            warm_threshold_days,
            cold_after,
        } => {
            format!(
                "AutoTier(hot<{hot_threshold_days}d warm<{warm_threshold_days}d cold>{cold_after}d)"
            )
        }
    }
}

fn format_hash(hash: &[u8; 32]) -> String {
    let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
    format!("{}…{}", &hex[..8], &hex[56..])
}

fn format_ns(ns: i64) -> String {
    if ns == 0 {
        return "—".into();
    }
    let secs = ns / 1_000_000_000;
    let ms = (ns % 1_000_000_000) / 1_000_000;
    format!("{secs}.{ms:03}s")
}

// ── App state ────────────────────────────────────────────────────────

struct AnalyzerApp {
    snap: PoolSnapshot,
    pool_path: String,

    // Selection
    selected_tag: Option<u32>,
    selected_object: Option<usize>,
    hovered_tag: Option<u32>,
    hovered_object: Option<usize>,

    // Filters
    tag_filter: String,
    object_filter: String,
    show_tombstoned: bool,

    // Tag/object label rects (for drawing connection lines)
    tag_rects: BTreeMap<u32, egui::Rect>,
    obj_rects: BTreeMap<usize, egui::Rect>,

    // Hex view state
    hex_cache: HexCache,

    // Hex field rects (populated during hex rendering, used for connection lines)
    /// Screen rects of named fields in the Object Record hex dump.
    record_field_rects: BTreeMap<String, egui::Rect>,
    /// Bounding rect of the Assertions hex bytes area.
    assertions_bytes_rect: Option<egui::Rect>,
}

/// Cached raw bytes for the hex viewer, updated on selection change.
struct HexCache {
    /// Which object index these bytes belong to (None = stale).
    cached_for: Option<usize>,
    /// Raw 128-byte ObjectRecord from disk.
    record_bytes: Vec<u8>,
    record_offset: u64,
    /// Serialized assertions (CBOR of this object's forward entries).
    assertions_bytes: Vec<u8>,
    /// Raw blob bytes (from in-memory store or disk).
    blob_bytes: Vec<u8>,
    blob_offset: u64,
}

impl HexCache {
    fn empty() -> Self {
        Self {
            cached_for: None,
            record_bytes: Vec::new(),
            record_offset: 0,
            assertions_bytes: Vec::new(),
            blob_bytes: Vec::new(),
            blob_offset: 0,
        }
    }
}

impl AnalyzerApp {
    fn new(snap: PoolSnapshot, pool_path: String) -> Self {
        Self {
            snap,
            pool_path,
            selected_tag: None,
            selected_object: None,
            hovered_tag: None,
            hovered_object: None,
            tag_filter: String::new(),
            object_filter: String::new(),
            show_tombstoned: false,
            tag_rects: BTreeMap::new(),
            obj_rects: BTreeMap::new(),
            hex_cache: HexCache::empty(),
            record_field_rects: BTreeMap::new(),
            assertions_bytes_rect: None,
        }
    }

    fn objects_for_tag(&self, tag_raw: u32) -> BTreeSet<usize> {
        let mut result = BTreeSet::new();
        for (idx, obj) in self.snap.objects.iter().enumerate() {
            if obj.direct_tags.contains(&tag_raw) || obj.materialized_tags.contains(&tag_raw) {
                result.insert(idx);
            }
        }
        result
    }

    fn tags_for_object(&self, obj_idx: usize) -> BTreeSet<u32> {
        let obj = &self.snap.objects[obj_idx];
        let mut set: BTreeSet<u32> = obj.direct_tags.iter().copied().collect();
        set.extend(obj.materialized_tags.iter());
        set
    }
}

impl eframe::App for AnalyzerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Top panel: pool summary
        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Mimisbrunnr Pool Analyzer");
                ui.separator();
                ui.label(&self.pool_path);
                ui.separator();
                ui.label(format!(
                    "node:{} | {} tags | {} objects | {} blobs ({})",
                    self.snap.node_id,
                    self.snap.tags.len(),
                    self.snap.objects.len(),
                    self.snap.blob_count,
                    format_bytes(self.snap.total_blob_bytes),
                ));
            });
        });

        // Bottom panel: placement rules & contexts
        egui::TopBottomPanel::bottom("footer").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if !self.snap.placement_rules.is_empty() {
                    ui.label("Rules:");
                    for rule in &self.snap.placement_rules {
                        ui.label(egui::RichText::new(rule).small().monospace());
                    }
                    ui.separator();
                }
                if !self.snap.contexts.is_empty() {
                    ui.label("Contexts:");
                    for ctx_info in &self.snap.contexts {
                        ui.label(
                            egui::RichText::new(format!(
                                "{} ({})",
                                ctx_info.name, ctx_info.entry_count
                            ))
                            .small(),
                        );
                    }
                }
                if self.snap.placement_rules.is_empty() && self.snap.contexts.is_empty() {
                    ui.weak("No placement rules or path contexts defined.");
                }
            });
        });

        // Bottom panel: detail view for selected object/tag
        // Central panel: Objects | Tags | Detail | (hex columns when selected)
        egui::CentralPanel::default().show(ctx, |ui| {
            self.tag_rects.clear();
            self.obj_rects.clear();

            if self.selected_object.is_some() {
                self.update_hex_cache();
            }

            let available_width = ui.available_width();
            let available_height = ui.available_height();
            let spacing = ui.spacing().item_spacing.x;

            // Objects | Tags | Record hex | Assertions hex | Blob hex | Detail
            // Always 6 columns; hex columns are zero-width when no object selected.
            let col_widths: [f32; 6] = if self.selected_object.is_some() {
                [0.10, 0.08, 0.22, 0.22, 0.20, 0.18]
            } else {
                [0.20, 0.20, 0.0, 0.0, 0.0, 0.60]
            };
            let active_cols = col_widths.iter().filter(|&&w| w > 0.0).count();
            let total_spacing = spacing * (active_cols as f32 - 1.0).max(0.0);
            let usable = available_width - total_spacing;

            let mut col_x = ui.min_rect().left();
            let top_y = ui.min_rect().top();

            // Helper: allocate a column rect, advancing col_x
            let make_col = |col_x: &mut f32, width_frac: f32| -> egui::Rect {
                let w = usable * width_frac;
                let rect = egui::Rect::from_min_size(
                    egui::pos2(*col_x, top_y),
                    egui::vec2(w, available_height),
                );
                *col_x += w + spacing;
                rect
            };

            // Column 0: Objects
            let rect0 = make_col(&mut col_x, col_widths[0]);
            let mut child0 = ui.new_child(egui::UiBuilder::new().max_rect(rect0));
            child0.set_clip_rect(rect0);
            self.draw_object_column(&mut child0);

            // Column 1: Tags
            let rect1 = make_col(&mut col_x, col_widths[1]);
            let mut child1 = ui.new_child(egui::UiBuilder::new().max_rect(rect1));
            child1.set_clip_rect(rect1);
            self.draw_tag_column(&mut child1);

            if self.selected_object.is_some() {
                // Column 2: Object Record hex
                let rect2 = make_col(&mut col_x, col_widths[2]);
                let mut child2 = ui.new_child(egui::UiBuilder::new().max_rect(rect2));
                child2.set_clip_rect(rect2);
                self.draw_hex_record_column(&mut child2);

                // Column 3: Assertions CBOR hex
                let rect3 = make_col(&mut col_x, col_widths[3]);
                let mut child3 = ui.new_child(egui::UiBuilder::new().max_rect(rect3));
                child3.set_clip_rect(rect3);
                self.draw_hex_assertions_column(&mut child3);

                // Column 4: Blob hex
                let rect4 = make_col(&mut col_x, col_widths[4]);
                let mut child4 = ui.new_child(egui::UiBuilder::new().max_rect(rect4));
                child4.set_clip_rect(rect4);
                self.draw_hex_blob_column(&mut child4);
            }

            // Column 5: Detail (always rightmost)
            let rect5 = make_col(&mut col_x, col_widths[5]);
            let mut child5 = ui.new_child(egui::UiBuilder::new().max_rect(rect5));
            child5.set_clip_rect(rect5);
            egui::ScrollArea::vertical()
                .id_salt("detail_scroll")
                .show(&mut child5, |ui| {
                    self.draw_detail_panel(ui);
                });
        });

        // Draw connection lines on top of everything (foreground layer)
        self.draw_connections(ctx);
    }
}

impl AnalyzerApp {
    fn draw_tag_column(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.strong("Tags");
            ui.add_space(8.0);
            ui.add(
                egui::TextEdit::singleline(&mut self.tag_filter)
                    .hint_text("filter…")
                    .desired_width(120.0),
            );
        });
        ui.separator();

        let filter_lower = self.tag_filter.to_lowercase();

        egui::ScrollArea::vertical()
            .id_salt("tags_scroll")
            .show(ui, |ui| {
                let filtered_indices: Vec<usize> = if filter_lower.is_empty() {
                    (0..self.snap.tags.len()).collect()
                } else {
                    self.snap
                        .tags
                        .iter()
                        .enumerate()
                        .filter(|(_, t)| t.name.to_lowercase().contains(&filter_lower))
                        .map(|(i, _)| i)
                        .collect()
                };

                for &idx in &filtered_indices {
                    let tag = &self.snap.tags[idx];
                    let tag_raw = tag.id.raw();
                    let is_selected = self.selected_tag == Some(tag_raw);
                    let is_highlighted = self
                        .selected_object
                        .is_some_and(|oi| self.tags_for_object(oi).contains(&tag_raw));

                    let indent = if tag.implied_by.is_empty() { 0.0 } else { 12.0 };

                    ui.horizontal(|ui| {
                        ui.add_space(indent);

                        let label_text = format!("{} ({})", tag.name, tag.object_count);
                        let mut text = egui::RichText::new(&label_text);
                        if is_selected {
                            text = text.strong().color(egui::Color32::from_rgb(80, 180, 255));
                        } else if is_highlighted {
                            text = text.strong().color(egui::Color32::from_rgb(120, 220, 160));
                        }

                        let response = ui.selectable_label(is_selected, text);
                        let rect = response.rect;
                        self.tag_rects.insert(tag_raw, rect);

                        if response.clicked() {
                            self.selected_tag = if is_selected { None } else { Some(tag_raw) };
                            // Keep selected_object so hex view stays visible
                        }

                        if response.hovered() {
                            self.hovered_tag = Some(tag_raw);
                            response.show_tooltip_ui(|ui| {
                                ui.label(
                                    egui::RichText::new(format!("Tag: {}", tag.name)).strong(),
                                );
                                ui.label(format!("ID: {}", tag_raw));
                                ui.label(format!("Semantics: {}", tag.semantics));
                                ui.label(format!("Objects: {}", tag.object_count));
                                if !tag.implies.is_empty() {
                                    let names: Vec<&str> = tag
                                        .implies
                                        .iter()
                                        .filter_map(|tid| {
                                            self.snap
                                                .tag_by_id
                                                .get(tid)
                                                .map(|&i| self.snap.tags[i].name.as_str())
                                        })
                                        .collect();
                                    ui.label(format!("Implies: {}", names.join(", ")));
                                }
                                if !tag.implied_by.is_empty() {
                                    let names: Vec<&str> = tag
                                        .implied_by
                                        .iter()
                                        .filter_map(|tid| {
                                            self.snap
                                                .tag_by_id
                                                .get(tid)
                                                .map(|&i| self.snap.tags[i].name.as_str())
                                        })
                                        .collect();
                                    ui.label(format!("Implied by: {}", names.join(", ")));
                                }
                            });
                        }
                    });
                }
            });
    }

    fn draw_object_column(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.strong("Objects");
            ui.add_space(8.0);
            ui.add(
                egui::TextEdit::singleline(&mut self.object_filter)
                    .hint_text("filter…")
                    .desired_width(120.0),
            );
            ui.checkbox(&mut self.show_tombstoned, "tombstoned");
        });
        ui.separator();

        let highlighted_objects: BTreeSet<usize> = self
            .selected_tag
            .map_or(BTreeSet::new(), |t| self.objects_for_tag(t));

        let filter_lower = self.object_filter.to_lowercase();

        egui::ScrollArea::vertical()
            .id_salt("objects_scroll")
            .show(ui, |ui| {
                for (idx, obj) in self.snap.objects.iter().enumerate() {
                    if !self.show_tombstoned && obj.state != ObjectState::Active {
                        continue;
                    }

                    // Apply text filter (matches on path, tag names, or object id)
                    if !filter_lower.is_empty() {
                        let id_str = obj.local.to_string();
                        let path_match = obj
                            .paths
                            .iter()
                            .any(|(_, p)| p.to_lowercase().contains(&filter_lower));
                        let tag_match = obj.direct_tags.iter().any(|tid| {
                            self.snap.tag_by_id.get(tid).is_some_and(|&i| {
                                self.snap.tags[i]
                                    .name
                                    .to_lowercase()
                                    .contains(&filter_lower)
                            })
                        });
                        if !id_str.contains(&filter_lower) && !path_match && !tag_match {
                            continue;
                        }
                    }

                    let is_selected = self.selected_object == Some(idx);
                    let is_highlighted = highlighted_objects.contains(&idx);

                    ui.horizontal(|ui| {
                        // Object label: show path if available, else ID
                        let display = if let Some((_, path)) = obj.paths.first() {
                            format!("#{} {}", obj.local, path)
                        } else {
                            format!("#{}", obj.local)
                        };

                        let mut text = egui::RichText::new(&display).monospace();
                        if is_selected {
                            text = text.strong().color(egui::Color32::from_rgb(100, 255, 150));
                        } else if is_highlighted {
                            text = text.strong().color(egui::Color32::from_rgb(80, 180, 255));
                        }
                        if obj.state != ObjectState::Active {
                            text = text.strikethrough();
                        }

                        let response = ui.selectable_label(is_selected, text);
                        let rect = response.rect;
                        self.obj_rects.insert(idx, rect);

                        if response.clicked() {
                            self.selected_object = if is_selected { None } else { Some(idx) };
                            // Keep selected_tag so connection lines stay visible
                        }

                        if response.hovered() {
                            self.hovered_object = Some(idx);
                        }

                        // Compact info after the label
                        if obj.has_blob {
                            ui.label(
                                egui::RichText::new(format_bytes(obj.blob_length))
                                    .small()
                                    .weak(),
                            );
                        }
                        if obj.compression != CompressionState::None {
                            ui.label(
                                egui::RichText::new(format!("{:?}", obj.compression))
                                    .small()
                                    .weak(),
                            );
                        }
                    });
                }
            });
    }

    fn draw_detail_panel(&self, ui: &mut egui::Ui) {
        if let Some(idx) = self.selected_object {
            self.draw_object_detail(ui, idx);
        } else if let Some(tag_raw) = self.selected_tag {
            self.draw_tag_detail(ui, tag_raw);
        } else {
            self.draw_pool_overview(ui);
        }
    }

    fn draw_pool_overview(&self, ui: &mut egui::Ui) {
        ui.heading("Pool Overview");
        ui.separator();

        ui.label(format!("Node ID: {}", self.snap.node_id));
        ui.label(format!(
            "Zones: idx@{:#x} meta@{:#x} blob@{:#x}",
            self.snap.index_zone_offset, self.snap.metadata_zone_offset, self.snap.blob_zone_offset,
        ));
        ui.label(format!("Tags: {}", self.snap.tags.len()));
        ui.label(format!("Objects: {}", self.snap.objects.len()));
        ui.label(format!(
            "Blobs: {} ({})",
            self.snap.blob_count,
            format_bytes(self.snap.total_blob_bytes)
        ));

        if !self.snap.implications.is_empty() {
            ui.add_space(8.0);
            ui.strong("Implications");
            for (from, to) in &self.snap.implications {
                let from_name = self
                    .snap
                    .tag_by_id
                    .get(from)
                    .map(|&i| self.snap.tags[i].name.as_str())
                    .unwrap_or("?");
                let to_name = self
                    .snap
                    .tag_by_id
                    .get(to)
                    .map(|&i| self.snap.tags[i].name.as_str())
                    .unwrap_or("?");
                ui.label(format!("  {from_name} → {to_name}"));
            }
        }

        // Disks from config
        if !self.snap.config.disks.is_empty() {
            ui.add_space(8.0);
            ui.strong("Disks");
            for disk in &self.snap.config.disks {
                ui.label(format!(
                    "  disk{}: {} ({}, {})",
                    disk.id,
                    disk.path,
                    disk.tier,
                    format_bytes(disk.capacity_bytes),
                ));
            }
        }

        if !self.snap.placement_rules.is_empty() {
            ui.add_space(8.0);
            ui.strong("Placement Rules");
            for rule in &self.snap.placement_rules {
                ui.label(format!("  {rule}"));
            }
        }

        ui.add_space(8.0);
        ui.weak("Click a tag or object to inspect it.");
    }

    fn draw_object_detail(&self, ui: &mut egui::Ui, idx: usize) {
        let obj = &self.snap.objects[idx];
        ui.heading(format!("Object #{} (node:{})", obj.local, obj.oid.node()));
        ui.separator();

        egui::Grid::new("obj_detail_grid")
            .num_columns(2)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                ui.label("State:");
                ui.label(format!("{:?}", obj.state));
                ui.end_row();

                ui.label("Raw ID:");
                ui.label(
                    egui::RichText::new(format!("{:#x}", obj.id_raw))
                        .monospace()
                        .small(),
                );
                ui.end_row();

                ui.label("Generation:");
                ui.label(format!("{}", obj.generation));
                ui.end_row();

                ui.label("Tags/Attrs:");
                ui.label(format!("{} / {}", obj.tag_count, obj.attr_count));
                ui.end_row();

                ui.label("Created:");
                ui.label(format_ns(obj.created_ns));
                ui.end_row();

                ui.label("Modified:");
                ui.label(format_ns(obj.modified_ns));
                ui.end_row();

                if obj.blob_length > 0 {
                    ui.label("Blob size:");
                    ui.label(format_bytes(obj.blob_length));
                    ui.end_row();

                    ui.label("Stored size:");
                    ui.label(format!(
                        "{} ({:?})",
                        format_bytes(obj.stored_size),
                        obj.compression
                    ));
                    ui.end_row();

                    let ratio = if obj.blob_length > 0 {
                        (obj.stored_size as f64 / obj.blob_length as f64) * 100.0
                    } else {
                        0.0
                    };
                    ui.label("Ratio:");
                    ui.label(format!("{ratio:.1}%"));
                    ui.end_row();
                }

                ui.label("Hash:");
                ui.label(
                    egui::RichText::new(format_hash(&obj.content_hash))
                        .monospace()
                        .small(),
                );
                ui.end_row();

                if obj.has_blob {
                    ui.label("Blob data:");
                    ui.label(format!(
                        "{} in memory",
                        format_bytes(obj.blob_size.unwrap_or(0) as u64)
                    ));
                    ui.end_row();
                }
            });

        // Direct tags
        if !obj.direct_tags.is_empty() {
            ui.add_space(8.0);
            ui.strong("Direct Tags");
            for tid in &obj.direct_tags {
                let name = self
                    .snap
                    .tag_by_id
                    .get(tid)
                    .map(|&i| self.snap.tags[i].name.as_str())
                    .unwrap_or("?");
                ui.label(format!("  {name} (id:{tid})"));
            }
        }

        // Materialized tags
        if !obj.materialized_tags.is_empty() {
            ui.add_space(4.0);
            ui.strong("Materialized Tags");
            for tid in &obj.materialized_tags {
                let name = self
                    .snap
                    .tag_by_id
                    .get(tid)
                    .map(|&i| self.snap.tags[i].name.as_str())
                    .unwrap_or("?");
                ui.label(
                    egui::RichText::new(format!("  {name} (id:{tid})"))
                        .weak()
                        .italics(),
                );
            }
        }

        // Attributes
        if !obj.attrs.is_empty() {
            ui.add_space(8.0);
            ui.strong("Attributes");
            for (key, val) in &obj.attrs {
                ui.label(format!("  {key} = {val}"));
            }
        }

        // Path projections
        if !obj.paths.is_empty() {
            ui.add_space(8.0);
            ui.strong("Path Projections");
            for (ctx, path) in &obj.paths {
                ui.label(format!("  [{ctx}] {path}"));
            }
        }
    }

    fn draw_tag_detail(&self, ui: &mut egui::Ui, tag_raw: u32) {
        let tag_idx = match self.snap.tag_by_id.get(&tag_raw) {
            Some(&i) => i,
            None => {
                ui.label("Tag not found");
                return;
            }
        };
        let tag = &self.snap.tags[tag_idx];

        ui.heading(format!("Tag: {}", tag.name));
        ui.separator();

        egui::Grid::new("tag_detail_grid")
            .num_columns(2)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                ui.label("ID:");
                ui.label(format!("{}", tag_raw));
                ui.end_row();

                ui.label("Semantics:");
                ui.label(&tag.semantics);
                ui.end_row();

                ui.label("Objects:");
                ui.label(format!("{}", tag.object_count));
                ui.end_row();
            });

        if !tag.implies.is_empty() {
            ui.add_space(8.0);
            ui.strong("Implies");
            for tid in &tag.implies {
                let name = self
                    .snap
                    .tag_by_id
                    .get(tid)
                    .map(|&i| self.snap.tags[i].name.as_str())
                    .unwrap_or("?");
                ui.label(format!("  → {name}"));
            }
        }

        if !tag.implied_by.is_empty() {
            ui.add_space(4.0);
            ui.strong("Implied By");
            for tid in &tag.implied_by {
                let name = self
                    .snap
                    .tag_by_id
                    .get(tid)
                    .map(|&i| self.snap.tags[i].name.as_str())
                    .unwrap_or("?");
                ui.label(format!("  ← {name}"));
            }
        }

        // List objects with this tag
        let obj_indices = self.objects_for_tag(tag_raw);
        if !obj_indices.is_empty() {
            ui.add_space(8.0);
            ui.strong("Tagged Objects");
            for &oi in &obj_indices {
                let obj = &self.snap.objects[oi];
                let display = if let Some((_, path)) = obj.paths.first() {
                    format!("  #{} {path}", obj.local)
                } else {
                    format!("  #{}", obj.local)
                };
                ui.label(egui::RichText::new(display).monospace().small());
            }
        }
    }

    fn draw_connections(&self, ctx: &egui::Context) {
        // Determine which connections to draw
        let connections: Vec<(u32, usize)> = if let Some(tag_raw) = self.selected_tag {
            self.objects_for_tag(tag_raw)
                .into_iter()
                .map(|oi| (tag_raw, oi))
                .collect()
        } else if let Some(obj_idx) = self.selected_object {
            let obj = &self.snap.objects[obj_idx];
            obj.direct_tags
                .iter()
                .chain(obj.materialized_tags.iter())
                .map(|&tid| (tid, obj_idx))
                .collect()
        } else {
            return;
        };

        // Paint on the foreground layer so lines appear on top of all panels
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("connection_lines"),
        ));

        // Deduplicate tags so each tag gets one consistent color index
        let unique_tags: Vec<u32> = {
            let mut seen = BTreeSet::new();
            connections
                .iter()
                .filter_map(|(t, _)| if seen.insert(*t) { Some(*t) } else { None })
                .collect()
        };
        let tag_color_index: HashMap<u32, usize> = unique_tags
            .iter()
            .enumerate()
            .map(|(i, &t)| (t, i))
            .collect();

        // Object → Tag lines (objects are left of tags now)
        for (tag_raw, obj_idx) in &connections {
            let tag_rect = self.tag_rects.get(tag_raw);
            let obj_rect = self.obj_rects.get(obj_idx);

            if let (Some(tr), Some(or)) = (tag_rect, obj_rect) {
                let from = egui::pos2(or.right(), or.center().y);
                let to = egui::pos2(tr.left(), tr.center().y);
                let mid_x = (from.x + to.x) / 2.0;

                let ci = tag_color_index.get(tag_raw).copied().unwrap_or(0);
                let is_materialized = self
                    .snap
                    .objects
                    .get(*obj_idx)
                    .is_some_and(|o| o.materialized_tags.contains(tag_raw));
                let alpha = if is_materialized { 140u8 } else { 200u8 };
                let color = line_color_for(ci, alpha);
                let width = if is_materialized { 1.5 } else { 2.0 };

                draw_bezier(&painter, from, to, mid_x, egui::Stroke::new(width, color));
            }
        }

        // Tag → inline_tags hex bytes (tags column → hex field)
        if let Some(inline_rect) = self.record_field_rects.get("inline_tags") {
            let mut drawn_highlight = false;
            for (tag_raw, _) in &connections {
                if let Some(tr) = self.tag_rects.get(tag_raw) {
                    let ci = tag_color_index.get(tag_raw).copied().unwrap_or(0);
                    let from = egui::pos2(tr.right(), tr.center().y);
                    let to = egui::pos2(inline_rect.left(), inline_rect.center().y);
                    let mid_x = (from.x + to.x) / 2.0;

                    let color = line_color_for(ci, 160);
                    draw_bezier(&painter, from, to, mid_x, egui::Stroke::new(1.5, color));

                    if !drawn_highlight {
                        let bg = egui::Color32::from_rgba_premultiplied(80, 80, 80, 40);
                        painter.rect_filled(inline_rect.expand(2.0), 2.0, bg);
                        drawn_highlight = true;
                    }
                }
            }
        }

        // Tag → Assertions bytes area
        if let Some(assert_rect) = self.assertions_bytes_rect {
            for (tag_raw, _) in &connections {
                if let Some(tr) = self.tag_rects.get(tag_raw) {
                    let ci = tag_color_index.get(tag_raw).copied().unwrap_or(0);
                    let from = egui::pos2(tr.right(), tr.center().y);
                    let to = egui::pos2(assert_rect.left(), assert_rect.center().y);
                    let mid_x = (from.x + to.x) / 2.0;

                    let color = line_color_for(ci, 100);
                    draw_bezier(&painter, from, to, mid_x, egui::Stroke::new(1.0, color));
                }
            }
        }

    }

    fn update_hex_cache(&mut self) {
        let obj_idx = match self.selected_object {
            Some(i) => i,
            None => return,
        };
        if self.hex_cache.cached_for == Some(obj_idx) {
            return;
        }

        let obj = &self.snap.objects[obj_idx];
        let disk_path = &self.snap.disk_path;

        // Read object record bytes from disk
        let mut record_bytes = vec![0u8; RECORD_SIZE];
        if let Ok(file) = std::fs::File::open(disk_path) {
            use std::io::{Read, Seek, SeekFrom};
            let mut file = file;
            if file.seek(SeekFrom::Start(obj.record_disk_offset)).is_ok() {
                let _ = file.read_exact(&mut record_bytes);
            }
        }

        // Serialize this object's assertions as CBOR for display
        let assertions_bytes = {
            let entries = &obj.direct_tags;
            let materialized = &obj.materialized_tags;
            let attrs = &obj.attrs;

            // Build a simple serializable representation
            #[derive(serde::Serialize)]
            struct AssertionDump {
                direct_tags: Vec<u32>,
                materialized_tags: Vec<u32>,
                attrs: Vec<(String, String)>,
            }
            let dump = AssertionDump {
                direct_tags: entries.clone(),
                materialized_tags: materialized.clone(),
                attrs: attrs.clone(),
            };
            let mut buf = Vec::new();
            let _ = ciborium::into_writer(&dump, &mut buf);
            buf
        };

        // Use blob preview from snapshot (already extracted from in-memory store)
        let blob_bytes = obj.blob_preview.clone();

        self.hex_cache = HexCache {
            cached_for: Some(obj_idx),
            record_bytes,
            record_offset: obj.record_disk_offset,
            assertions_bytes,
            blob_bytes,
            blob_offset: obj.blob_disk_offset,
        };
    }

    fn draw_hex_record_column(&mut self, ui: &mut egui::Ui) {
        self.record_field_rects.clear();

        ui.strong("Object Record");
        ui.label(
            egui::RichText::new(format!(
                "offset {:#010x}  ({} bytes)",
                self.hex_cache.record_offset, RECORD_SIZE
            ))
            .small()
            .weak(),
        );
        ui.separator();
        let fields = record_field_ranges();
        egui::ScrollArea::vertical()
            .id_salt("hex_record")
            .show(ui, |ui| {
                let rects = draw_hex_dump(
                    ui,
                    &self.hex_cache.record_bytes,
                    self.hex_cache.record_offset,
                    Some(&fields),
                );
                self.record_field_rects = rects;
            });
    }

    fn draw_hex_assertions_column(&mut self, ui: &mut egui::Ui) {
        self.assertions_bytes_rect = None;

        ui.strong("Assertions (CBOR)");
        ui.label(
            egui::RichText::new(format!(
                "serialized  ({} bytes)",
                self.hex_cache.assertions_bytes.len()
            ))
            .small()
            .weak(),
        );
        ui.separator();
        let assert_response =
            egui::ScrollArea::vertical()
                .id_salt("hex_assertions")
                .show(ui, |ui| {
                    draw_hex_dump(ui, &self.hex_cache.assertions_bytes, 0, None);
                });
        self.assertions_bytes_rect = Some(assert_response.inner_rect);
    }

    fn draw_hex_blob_column(&mut self, ui: &mut egui::Ui) {

        ui.strong("Blob Data");
        if !self.hex_cache.blob_bytes.is_empty() {
            ui.label(
                egui::RichText::new(format!(
                    "offset {:#010x}  ({} bytes shown)",
                    self.hex_cache.blob_offset,
                    self.hex_cache.blob_bytes.len()
                ))
                .small()
                .weak(),
            );
        } else {
            ui.label(egui::RichText::new("no blob data").small().weak());
        }
        ui.separator();
        egui::ScrollArea::vertical()
            .id_salt("hex_blob")
            .show(ui, |ui| {
                draw_hex_dump(
                    ui,
                    &self.hex_cache.blob_bytes,
                    self.hex_cache.blob_offset,
                    None,
                );
            });
    }
}

// ── Hex dump rendering ──────────────────────────────────────────────

/// A named byte range for highlighting in the hex dump.
/// A palette of 12 visually distinct colors for connection lines.
const LINE_PALETTE: [egui::Color32; 12] = [
    egui::Color32::from_rgb(230, 25, 75),   // red
    egui::Color32::from_rgb(60, 180, 75),   // green
    egui::Color32::from_rgb(0, 130, 200),   // blue
    egui::Color32::from_rgb(245, 130, 48),  // orange
    egui::Color32::from_rgb(145, 30, 180),  // purple
    egui::Color32::from_rgb(70, 240, 240),  // cyan
    egui::Color32::from_rgb(240, 50, 230),  // magenta
    egui::Color32::from_rgb(210, 245, 60),  // lime
    egui::Color32::from_rgb(250, 190, 212), // pink
    egui::Color32::from_rgb(0, 128, 128),   // teal
    egui::Color32::from_rgb(220, 190, 255), // lavender
    egui::Color32::from_rgb(170, 110, 40),  // brown
];

/// Get a distinct line color for a tag (by index in the connection list).
fn line_color_for(index: usize, alpha: u8) -> egui::Color32 {
    let base = LINE_PALETTE[index % LINE_PALETTE.len()];
    egui::Color32::from_rgba_premultiplied(
        (base.r() as u16 * alpha as u16 / 255) as u8,
        (base.g() as u16 * alpha as u16 / 255) as u8,
        (base.b() as u16 * alpha as u16 / 255) as u8,
        alpha,
    )
}

/// Draw a cubic bezier curve between two points.
fn draw_bezier(
    painter: &egui::Painter,
    from: egui::Pos2,
    to: egui::Pos2,
    mid_x: f32,
    stroke: egui::Stroke,
) {
    painter.add(egui::Shape::CubicBezier(
        egui::epaint::CubicBezierShape::from_points_stroke(
            [from, egui::pos2(mid_x, from.y), egui::pos2(mid_x, to.y), to],
            false,
            egui::Color32::TRANSPARENT,
            stroke,
        ),
    ));
}

struct FieldRange {
    name: &'static str,
    start: usize,
    len: usize,
    color: egui::Color32,
}

/// Field ranges for the ObjectRecord layout (matches #[repr(C)] Pod struct).
fn record_field_ranges() -> Vec<FieldRange> {
    vec![
        FieldRange {
            name: "id",
            start: 0,
            len: 8,
            color: egui::Color32::from_rgb(120, 180, 255),
        },
        FieldRange {
            name: "blob_offset",
            start: 8,
            len: 8,
            color: egui::Color32::from_rgb(255, 180, 120),
        },
        FieldRange {
            name: "blob_length",
            start: 16,
            len: 8,
            color: egui::Color32::from_rgb(255, 220, 120),
        },
        FieldRange {
            name: "stored_size",
            start: 24,
            len: 8,
            color: egui::Color32::from_rgb(200, 255, 120),
        },
        FieldRange {
            name: "overflow_off",
            start: 32,
            len: 8,
            color: egui::Color32::from_rgb(180, 180, 255),
        },
        FieldRange {
            name: "created_ns",
            start: 40,
            len: 8,
            color: egui::Color32::from_rgb(255, 160, 200),
        },
        FieldRange {
            name: "modified_ns",
            start: 48,
            len: 8,
            color: egui::Color32::from_rgb(255, 200, 200),
        },
        FieldRange {
            name: "generation",
            start: 56,
            len: 4,
            color: egui::Color32::from_rgb(200, 200, 255),
        },
        FieldRange {
            name: "inline_tags",
            start: 60,
            len: 16,
            color: egui::Color32::from_rgb(120, 255, 200),
        },
        FieldRange {
            name: "tag_count",
            start: 76,
            len: 2,
            color: egui::Color32::from_rgb(200, 255, 255),
        },
        FieldRange {
            name: "attr_count",
            start: 78,
            len: 2,
            color: egui::Color32::from_rgb(200, 255, 255),
        },
        FieldRange {
            name: "state",
            start: 80,
            len: 1,
            color: egui::Color32::from_rgb(255, 120, 120),
        },
        FieldRange {
            name: "compression",
            start: 81,
            len: 1,
            color: egui::Color32::from_rgb(255, 160, 120),
        },
        FieldRange {
            name: "encryption",
            start: 82,
            len: 1,
            color: egui::Color32::from_rgb(255, 200, 160),
        },
        FieldRange {
            name: "pad",
            start: 83,
            len: 1,
            color: egui::Color32::from_rgb(100, 100, 100),
        },
        FieldRange {
            name: "content_hash",
            start: 84,
            len: 32,
            color: egui::Color32::from_rgb(180, 120, 255),
        },
        FieldRange {
            name: "reserved",
            start: 116,
            len: 4,
            color: egui::Color32::from_rgb(80, 80, 80),
        },
        FieldRange {
            name: "compressed_size",
            start: 120,
            len: 8,
            color: egui::Color32::from_rgb(200, 160, 120),
        },
    ]
}

fn field_at_offset(fields: &[FieldRange], offset: usize) -> Option<&FieldRange> {
    fields
        .iter()
        .find(|f| offset >= f.start && offset < f.start + f.len)
}

/// Draw an ImHex-style hex dump with offset, hex bytes, and ASCII columns.
/// Returns a map of field name → bounding rect (only when `fields` is provided).
fn draw_hex_dump(
    ui: &mut egui::Ui,
    data: &[u8],
    base_offset: u64,
    fields: Option<&Vec<FieldRange>>,
) -> BTreeMap<String, egui::Rect> {
    let mut field_rects: BTreeMap<String, egui::Rect> = BTreeMap::new();

    if data.is_empty() {
        ui.weak("(empty)");
        return field_rects;
    }

    let painter = ui.painter().clone();
    // 8 bytes per row keeps columns from overlapping in a 3-column layout.
    // Row format: "041ef300 06 00 00 00 00 00 00 00 ........" ≈ 42 chars
    let bytes_per_row = 8;

    for (row_idx, chunk) in data.chunks(bytes_per_row).enumerate() {
        let row_offset = row_idx * bytes_per_row;
        let abs_offset = base_offset + row_offset as u64;

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;

            // Offset column (shorter: 6 hex digits for compactness)
            ui.label(
                egui::RichText::new(format!("{abs_offset:06x} "))
                    .monospace()
                    .weak(),
            );

            // Hex bytes
            for (i, &byte) in chunk.iter().enumerate() {
                let byte_offset = row_offset + i;
                let field = fields.and_then(|f| field_at_offset(f, byte_offset));
                let color = field
                    .map(|f| f.color)
                    .unwrap_or(egui::Color32::from_rgb(80, 80, 90));

                let hex_text = if i == 3 {
                    format!("{byte:02x}  ") // extra space at midpoint
                } else {
                    format!("{byte:02x} ")
                };

                let label = ui.label(egui::RichText::new(hex_text).monospace().color(color));

                // Paint a subtle background behind fields
                if let Some(f) = field {
                    let bg = egui::Color32::from_rgba_premultiplied(
                        f.color.r() / 5,
                        f.color.g() / 5,
                        f.color.b() / 5,
                        50,
                    );
                    painter.rect_filled(label.rect, 0.0, bg);

                    // Accumulate bounding rect for this field
                    let entry = field_rects.entry(f.name.to_string()).or_insert(label.rect);
                    *entry = entry.union(label.rect);
                }

                // Tooltip on hover showing field name + decoded value
                if let Some(f) = field
                    && label.hovered()
                {
                    label.show_tooltip_ui(|ui| {
                        ui.label(egui::RichText::new(f.name).strong());
                        ui.label(format!(
                            "offset: {:#x}..{:#x} ({} bytes)",
                            f.start,
                            f.start + f.len,
                            f.len
                        ));
                        if f.start + f.len <= data.len() {
                            let field_bytes = &data[f.start..f.start + f.len];
                            let decoded = decode_field(f.name, field_bytes);
                            ui.label(format!("value: {decoded}"));
                        }
                    });
                }
            }

            // Pad if short row
            if chunk.len() < bytes_per_row {
                let missing = bytes_per_row - chunk.len();
                let pad = " ".repeat(missing * 3 + if chunk.len() <= 3 { 1 } else { 0 });
                ui.label(egui::RichText::new(pad).monospace());
            }

            ui.label(egui::RichText::new(" ").monospace());

            // ASCII column
            let ascii: String = chunk
                .iter()
                .map(|&b| {
                    if (0x20..=0x7e).contains(&b) {
                        b as char
                    } else {
                        '.'
                    }
                })
                .collect();

            let ascii_color = egui::Color32::from_rgb(100, 100, 110);
            ui.label(egui::RichText::new(ascii).monospace().color(ascii_color));
        });
    }

    field_rects
}

/// Decode a field's raw bytes into a human-readable string.
fn decode_field(name: &str, bytes: &[u8]) -> String {
    match name {
        "id" | "blob_offset" | "blob_length" | "stored_size" | "overflow_off"
        | "compressed_size" => {
            if bytes.len() == 8 {
                let v = u64::from_le_bytes(bytes.try_into().unwrap());
                if v == 0 {
                    "0".into()
                } else {
                    format!("{v} ({v:#x})")
                }
            } else {
                format!("{bytes:02x?}")
            }
        }
        "created_ns" | "modified_ns" => {
            if bytes.len() == 8 {
                let v = i64::from_le_bytes(bytes.try_into().unwrap());
                format_ns(v)
            } else {
                format!("{bytes:02x?}")
            }
        }
        "generation" => {
            if bytes.len() == 4 {
                format!("{}", u32::from_le_bytes(bytes.try_into().unwrap()))
            } else {
                format!("{bytes:02x?}")
            }
        }
        "inline_tags" => {
            if bytes.len() == 16 {
                let tags: Vec<u32> = bytes
                    .chunks_exact(4)
                    .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
                    .collect();
                format!("{tags:?}")
            } else {
                format!("{bytes:02x?}")
            }
        }
        "tag_count" | "attr_count" => {
            if bytes.len() == 2 {
                format!("{}", u16::from_le_bytes(bytes.try_into().unwrap()))
            } else {
                format!("{bytes:02x?}")
            }
        }
        "state" => match bytes.first() {
            Some(0) => "Active (0)".into(),
            Some(1) => "Tombstoned (1)".into(),
            Some(2) => "BlobReclaim (2)".into(),
            Some(3) => "Cleared (3)".into(),
            Some(v) => format!("unknown ({v})"),
            None => "?".into(),
        },
        "compression" => match bytes.first() {
            Some(0) => "None (0)".into(),
            Some(1) => "Zstd (1)".into(),
            Some(2) => "Lz4 (2)".into(),
            Some(v) => format!("unknown ({v})"),
            None => "?".into(),
        },
        "encryption" => match bytes.first() {
            Some(0) => "None (0)".into(),
            Some(1) => "Hctr2Aes128 (1)".into(),
            Some(2) => "XtsAes256 (2)".into(),
            Some(v) => format!("unknown ({v})"),
            None => "?".into(),
        },
        "content_hash" => {
            let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
            if hex.len() > 16 {
                format!("{}…{}", &hex[..8], &hex[hex.len() - 8..])
            } else {
                hex
            }
        }
        _ => format!("{bytes:02x?}"),
    }
}

fn format_bytes(bytes: u64) -> String {
    if bytes == 0 {
        return "0 B".into();
    }
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];
    let mut val = bytes as f64;
    let mut unit_idx = 0;
    while val >= 1024.0 && unit_idx < UNITS.len() - 1 {
        val /= 1024.0;
        unit_idx += 1;
    }
    if unit_idx == 0 {
        format!("{bytes} B")
    } else {
        format!("{val:.1} {}", UNITS[unit_idx])
    }
}

// ── Main ─────────────────────────────────────────────────────────────

fn main() -> eframe::Result {
    env_logger::init();
    let cli = Cli::parse();

    // Load the pool
    let config = PoolConfig::load(&cli.pool).unwrap_or_else(|e| {
        eprintln!("Failed to load pool config {:?}: {e}", cli.pool);
        std::process::exit(1);
    });

    let de = DiskEngine::open(&cli.pool).unwrap_or_else(|e| {
        eprintln!("Failed to open pool: {e}");
        std::process::exit(1);
    });

    let mut snap = extract_snapshot(&de);
    snap.config = config;
    snap.default_compression = snap.config.default_compression.clone();
    snap.placement_rules = de.engine().rules().iter().map(format_rule).collect();

    let pool_path = cli.pool.display().to_string();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1600.0, 900.0])
            .with_title("Mímisbrunnr Pool Analyzer"),
        ..Default::default()
    };

    eframe::run_native(
        "Mímisbrunnr Analyzer",
        options,
        Box::new(|_cc| Ok(Box::new(AnalyzerApp::new(snap, pool_path)))),
    )
}
