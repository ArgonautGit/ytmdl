//! Listening insights (crate::insights): their Settings section and menus,
//! the Insights page, its card on the stats page, and Ask, a chat about the
//! listening that is gone when its page closes.

use std::collections::HashMap;

use dioxus::prelude::*;
use ytmdl_core::{Entry, SearchSource};
use ytmdl_library::Library;

use super::icons::{Icon, Svg};
use super::views::{Cover, Dialog, MenuHead, MenuItem, MenuSheet, TrackState, dotted, plural};
use super::{Ctx, Overlay, Sheet, Songs, Tab, track_states, with_states};
use crate::insights::{
    self, Chat, Claude, Effort, Frequency, Model, Outcome, Pending, Pick, Report, Settings, Step, api_key, masked_key,
};
use crate::platform;

/// Insights' state, in [`Ctx`].
#[derive(Clone, Copy)]
pub(super) struct Insights {
    settings: Signal<Settings>,
    report: Signal<Option<Report>>,
    /// A report sent to the Batch API.
    pending: Signal<Option<Pending>>,
    key: Signal<Option<String>>,
    /// What a report run is doing, while one runs.
    running: Signal<Option<String>>,
    /// Why the last run failed.
    error: Signal<Option<String>>,
    /// Songs found on YouTube Music for picks, by [`pick_key`]; `None`: not found.
    found: Signal<HashMap<String, Option<Entry>>>,
}

impl Insights {
    pub(super) fn load(library: &Library) -> Insights {
        if let Ok(offset) = library.utc_offset() {
            insights::set_utc_offset(offset);
        }
        let (settings, report, pending, key) =
            (Settings::load(library), Report::load(library), Pending::load(library), api_key(library));
        Insights {
            settings: Signal::new(settings),
            report: Signal::new(report),
            pending: Signal::new(pending),
            key: Signal::new(key),
            running: Signal::new(None),
            error: Signal::new(None),
            found: Signal::new(HashMap::new()),
        }
    }
}

fn pick_key(p: &Pick) -> String {
    format!("{}\u{0}{}", p.title.to_lowercase(), p.artist.to_lowercase())
}

/// "Sonnet 5.5, medium"
fn model_text(model: Model, effort: Effort) -> String {
    format!("{}, {}", model.label(), effort.label().to_lowercase())
}

/// "$0.04", "under 1¢".
fn cost_text(dollars: f64) -> String {
    match dollars {
        d if d < 0.01 => "under 1¢".into(),
        d if d < 1.0 => format!("{:.0}¢", d * 100.0),
        d => format!("${d:.2}"),
    }
}

impl Ctx {
    fn set_insights(&self, change: impl FnOnce(&mut Settings)) {
        let mut settings = self.insights.settings;
        let mut next = settings.peek().clone();
        change(&mut next);
        if let Err(e) = next.save(&self.library.get()) {
            self.notify(format!("Couldn't save the setting: {e}"));
            return;
        }
        settings.set(next);
    }

    fn set_api_key(&self, key: String) {
        if let Err(e) = insights::set_api_key(&self.library.get(), &key) {
            self.notify(format!("Couldn't save the key: {e}"));
            return;
        }
        let mut k = self.insights.key;
        k.set(Some(insights::clean_key(&key)).filter(|k| !k.is_empty()));
        let Some(claude) = self.claude() else { return };
        let ctx = *self;
        // The dialog that saved it is gone by now: not its task.
        dioxus::core::spawn_forever(async move {
            match claude.check().await {
                Ok(()) => ctx.notify("The API key works".into()),
                Err(e) => {
                    let mut error = ctx.insights.error;
                    error.set(Some(e.to_string()));
                    ctx.notify(e.to_string());
                }
            }
        });
    }

    fn claude(&self) -> Option<Claude> {
        let svc = self.services()?;
        let settings = self.insights.settings.peek().clone();
        Some(Claude {
            rt: svc.dl.runtime().clone(),
            library: self.library.get(),
            key: self.insights.key.peek().clone()?,
            model: settings.model,
            effort: settings.effort,
        })
    }

    /// Makes a new report (or sends one to the Batch API) with the settings.
    pub(super) fn refresh_insights(&self) {
        let s = self.insights;
        if s.running.peek().is_some() || s.pending.peek().is_some() {
            return;
        }
        let Some(svc) = self.services() else { return };
        if s.key.peek().is_none() {
            self.notify("Add your Claude API key in Settings first".into());
            return;
        }
        let ctx = *self;
        let settings = s.settings.peek().clone();
        let (mut running, mut error, mut report, mut pending) = (s.running, s.error, s.report, s.pending);
        running.set(Some("Reading your history…".into()));
        error.set(None);
        spawn(async move {
            let mut queries = 0;
            let mut on_step = |step: Step| match step {
                Step::Query(sql) => {
                    tracing::debug!(target: "ytmdl", "insights query: {sql}");
                    queries += 1;
                    running.set(Some(format!("Looking into your history ({})…", plural(queries, "query", "queries"))));
                }
            };
            ctx.library.import_listens().await;
            let result = insights::refresh(svc.dl.runtime().clone(), ctx.library.get(), &settings, &mut on_step).await;
            running.set(None);
            match result {
                Ok(Outcome::Made(r)) => report.set(Some(r)),
                Ok(Outcome::Sent(p)) => pending.set(Some(p)),
                Err(e) => {
                    tracing::warn!(target: "ytmdl", "insights: {e:#}");
                    error.set(Some(format!("{e:#}")));
                }
            }
        });
    }

    /// Checks on a report sent to the Batch API.
    fn poll_insights(&self) {
        let s = self.insights;
        if s.pending.peek().is_none() || s.running.peek().is_some() {
            return;
        }
        let Some(svc) = self.services() else { return };
        let ctx = *self;
        let (mut running, mut error, mut report, mut pending) = (s.running, s.error, s.report, s.pending);
        running.set(Some("Checking on the batch…".into()));
        spawn(async move {
            let result = insights::poll(svc.dl.runtime().clone(), ctx.library.get()).await;
            running.set(None);
            match result {
                Ok(None) => {}
                Ok(Some(r)) => {
                    report.set(Some(r));
                    pending.set(None);
                    if *ctx.tab.peek() != Tab::Settings {
                        ctx.notify("Your listening insights are ready".into());
                    }
                }
                Err(e) => {
                    tracing::warn!(target: "ytmdl", "insights batch: {e:#}");
                    // Dropped by poll() unless it couldn't be asked.
                    pending.set(Pending::load(&ctx.library.get()));
                    error.set(Some(format!("{e:#}")));
                }
            }
        });
    }

    /// Run whenever the background work wakes: collects a batch, or starts a
    /// report when one is due.
    pub(super) fn insights_tick(&self) {
        let s = self.insights;
        if s.pending.peek().is_some() {
            self.poll_insights();
            return;
        }
        let settings = s.settings.peek().clone();
        let due = insights::due(&settings, s.report.peek().as_ref(), false, insights::now_secs());
        if due && s.key.peek().is_some() && s.error.peek().is_none() && (!settings.wifi_only || platform::unmetered_network()) {
            tracing::info!(target: "ytmdl", "insights: a {} report is due", settings.frequency.label().to_lowercase());
            self.refresh_insights();
        }
    }
}

/// The Listening history and Insights sections of Settings.
#[component]
pub(super) fn InsightsSettings() -> Element {
    let ctx = use_context::<Ctx>();
    let lib = ctx.library;
    let info = use_resource(move || {
        lib.subscribe_listens();
        let library = lib.get();
        async move { crate::blocking(move || library.history_info()).await }
    });
    let history = match &*info.read() {
        Some(Ok(Ok(i))) if i.listens > 0 => {
            let since = i.first.map(|t| format!(" since {}", insights::month_year(t))).unwrap_or_default();
            let n = insights::count_text(i.listens);
            let listens = if i.listens == 1 { "listen" } else { "listens" };
            format!("{n} {listens}{since}: what played, for how long, how it ended, where from and through what")
        }
        Some(_) => "Nothing played yet".to_string(),
        None => "Counting…".to_string(),
    };
    let s = ctx.insights;
    let settings = (s.settings)();
    let key = (s.key)();
    let report = (s.report)();
    let switch = |on: bool, label: &'static str, onclick: EventHandler<()>| {
        rsx! {
            button {
                class: if on { "switch on" } else { "switch" },
                role: "switch",
                "aria-checked": "{on}",
                "aria-label": "{label}",
                onclick: move |_| onclick.call(()),
                span { class: "knob" }
            }
        }
    };
    rsx! {
        section { class: "group",
            h2 { "Listening history" }
            div { class: "card",
                div { class: "item",
                    Svg { icon: Icon::Chart }
                    div { class: "meta",
                        div { class: "title", "Everything you play is kept" }
                        div { class: "sub wrap", "{history}" }
                    }
                }
                div { class: "item tappable divided", role: "button", onclick: move |_| export_history(ctx),
                    Svg { icon: Icon::Download }
                    div { class: "meta",
                        div { class: "title", "Export history" }
                        div { class: "sub", "As JSON, to the save location" }
                    }
                }
            }
        }
        section { class: "group",
            h2 { "Insights" }
            div { class: "card",
                div { class: "item",
                    Svg { icon: Icon::Sparkles }
                    div { class: "meta",
                        div { class: "title", "Listening insights" }
                        div { class: "sub wrap", "Claude reads your listening history, points out patterns in it and suggests music" }
                    }
                    {switch(settings.enabled, "Listening insights", EventHandler::new(move |_| ctx.set_insights(|s| s.enabled = !s.enabled)))}
                }
                if settings.enabled {
                    SettingRow {
                        title: "Claude API key",
                        sub: match &key {
                            Some(k) => format!("{} · kept on this phone", masked_key(k)),
                            None => "Needed: from console.anthropic.com. Max plans' monthly API credits pay for it".into(),
                        },
                        divided: true,
                        onclick: move |_| ctx.nav.push(Overlay::Sheet(Sheet::ApiKey)),
                    }
                    SettingRow {
                        title: "Model",
                        sub: model_text(settings.model, settings.effort),
                        onclick: move |_| ctx.nav.push(Overlay::Sheet(Sheet::InsightsModel)),
                    }
                    SettingRow {
                        title: "How often",
                        sub: dotted([
                            Some(settings.frequency.label().to_string()),
                            (settings.wifi_only && settings.frequency != Frequency::Manual).then(|| "on Wi-Fi".to_string()),
                        ]),
                        onclick: move |_| ctx.nav.push(Overlay::Sheet(Sheet::InsightsFrequency)),
                    }
                    div { class: "item",
                        span { class: "icon spacer" }
                        div { class: "meta",
                            div { class: "title", "Half price (Batch API)" }
                            div { class: "sub wrap",
                                if settings.batch {
                                    "Reports come within about an hour, from a summary of your history"
                                } else {
                                    "Reports come in a minute or two; Claude runs its own queries on your history"
                                }
                            }
                        }
                        {switch(settings.batch, "Half price (Batch API)", EventHandler::new(move |_| ctx.set_insights(|s| s.batch = !s.batch)))}
                    }
                    if let Some(r) = report {
                        div { class: "item tappable", role: "button", onclick: move |_| ctx.nav.push(Overlay::Insights),
                            span { class: "icon spacer" }
                            div { class: "meta",
                                div { class: "title", "Latest insights" }
                                div { class: "sub",
                                    {dotted([Some(insights::day_text(r.made_at)), Some(model_text(r.model, r.effort)), Some(cost_text(r.cost))])}
                                }
                            }
                            span { class: "chevron", Svg { icon: Icon::ChevronRight, size: 20 } }
                        }
                    }
                }
            }
            p { class: "hint",
                "Sent to Anthropic when a report is made or you ask: titles, artists, albums and when and how you played them. Nothing while this is off."
            }
        }
    }
}

#[component]
fn SettingRow(title: String, sub: String, #[props(default)] divided: bool, onclick: EventHandler<()>) -> Element {
    rsx! {
        div { class: if divided { "item tappable divided" } else { "item tappable" }, role: "button", onclick: move |_| onclick.call(()),
            span { class: "icon spacer" }
            div { class: "meta",
                div { class: "title", "{title}" }
                div { class: "sub wrap", "{sub}" }
            }
            span { class: "chevron", Svg { icon: Icon::ChevronRight, size: 20 } }
        }
    }
}

fn export_history(ctx: Ctx) {
    let Some(svc) = ctx.services() else { return };
    let dir = if *ctx.storage.peek() { svc.output_dir.clone() } else { svc.fallback_output_dir.clone() };
    let path = dir.join("ytmdl-listening-history.json");
    let library = ctx.library.get();
    spawn(async move {
        ctx.library.import_listens().await;
        let shown = path.display().to_string().replacen("/storage/emulated/0/", "", 1);
        match crate::blocking(move || library.export_history(&path)).await {
            Ok(Ok(n)) => ctx.notify(format!("Saved {} to {shown}", plural(n, "listen", "listens"))),
            Ok(Err(e)) => ctx.notify(format!("Couldn't export: {e}")),
            Err(e) => ctx.notify(format!("Couldn't export: {e:#}")),
        }
    });
}

/// Model and effort.
#[component]
pub(super) fn ModelMenu() -> Element {
    let ctx = use_context::<Ctx>();
    let s = ctx.insights.settings.read().clone();
    let mut items: Vec<MenuItem> = Model::ALL
        .iter()
        .map(|&m| MenuItem { sub: Some(m.note().into()), ..MenuItem::choice(m.label(), m == s.model) })
        .collect();
    items.extend(Effort::ALL.iter().map(|&e| MenuItem {
        sub: Some(
            match e {
                Effort::Low => "Effort: quickest",
                Effort::Medium => "Effort: recommended",
                Effort::High => "Effort: thinks longer, costs more",
                Effort::ExtraHigh => "Effort: thinks longer still",
                Effort::Max => "Effort: thinks longest, costs most",
            }
            .into(),
        ),
        ..MenuItem::choice(e.label(), e == s.effort)
    }));
    rsx! {
        MenuSheet {
            head: MenuHead { title: "Model and effort".into(), sub: model_text(s.model, s.effort), art: None, icon: Icon::Sparkles },
            items,
            onclose: move |_| ctx.nav.back(),
            onpick: move |i: usize| {
                ctx.nav.back();
                match Model::ALL.get(i) {
                    Some(&m) => ctx.set_insights(|s| s.model = m),
                    None => {
                        if let Some(&e) = Effort::ALL.get(i - Model::ALL.len()) {
                            ctx.set_insights(|s| s.effort = e);
                        }
                    }
                }
            },
        }
    }
}

#[component]
pub(super) fn FrequencyMenu() -> Element {
    let ctx = use_context::<Ctx>();
    let s = ctx.insights.settings.read().clone();
    let mut items: Vec<MenuItem> = Frequency::ALL
        .iter()
        .map(|&f| {
            let sub = match f {
                Frequency::Manual => "Only when you tap Refresh",
                Frequency::Daily => "When you open the app, once a day",
                Frequency::Weekly => "When you open the app, once a week",
                Frequency::Monthly => "When you open the app, once a month",
            };
            MenuItem { sub: Some(sub.into()), ..MenuItem::choice(f.label(), f == s.frequency) }
        })
        .collect();
    items.push(MenuItem {
        sub: Some(if s.wifi_only { "On: not over mobile data" } else { "Off: over mobile data too" }.into()),
        checked: s.wifi_only,
        ..MenuItem::new(Icon::Sync, "Only on Wi-Fi")
    });
    rsx! {
        MenuSheet {
            head: MenuHead { title: "How often".into(), sub: "New insights from your listening".into(), art: None, icon: Icon::Sync },
            items,
            onclose: move |_| ctx.nav.back(),
            onpick: move |i: usize| {
                ctx.nav.back();
                match Frequency::ALL.get(i) {
                    Some(&f) => ctx.set_insights(|s| s.frequency = f),
                    None => ctx.set_insights(|s| s.wifi_only = !s.wifi_only),
                }
            },
        }
    }
}

#[component]
pub(super) fn ApiKeyDialog() -> Element {
    let ctx = use_context::<Ctx>();
    let mut value = use_signal(String::new);
    let has = ctx.insights.key.peek().is_some();
    rsx! {
        Dialog {
            title: "Claude API key",
            text: if has {
                "Paste a new key to replace the one kept on this phone.".to_string()
            } else {
                "Create one at console.anthropic.com (API keys), in the organization your plan's API credits go to.".to_string()
            },
            value: value(),
            placeholder: "sk-ant-…",
            max_length: 500,
            code: true,
            confirm: "Save",
            oninput: move |v| value.set(v),
            oncancel: move |_| ctx.nav.back(),
            onconfirm: move |_| {
                ctx.nav.back();
                let mut error = ctx.insights.error;
                error.set(None);
                ctx.set_api_key(value.peek().clone());
            },
        }
    }
}

/// The latest insight, on the stats page.
#[component]
pub(super) fn InsightsTeaser() -> Element {
    let ctx = use_context::<Ctx>();
    let s = ctx.insights;
    if !(s.settings)().enabled {
        return rsx! {};
    }
    let report = (s.report)();
    let pending = (s.pending)().is_some() || (s.running)().is_some();
    rsx! {
        div { class: "insight-teaser tappable", role: "button", onclick: move |_| ctx.nav.push(Overlay::Insights),
            div { class: "it-head",
                Svg { icon: Icon::Sparkles, size: 18 }
                "Insights"
                if let Some(r) = &report {
                    span { class: "it-when", {dotted([Some(insights::day_text(r.made_at)), Some(r.model.label().to_string())])} }
                }
            }
            match &report {
                Some(r) => rsx! {
                    div { class: "it-body", "{r.headline}" }
                    div { class: "it-foot",
                        {format!("{} · {} for you", plural(r.observations.len(), "observation", "observations"), plural(r.songs.len(), "song", "songs"))}
                        Svg { icon: Icon::ChevronRight, size: 18 }
                    }
                },
                None if pending => rsx! { div { class: "it-body", "Claude is looking at your listening…" } },
                None => rsx! { div { class: "it-body", "Get Claude's read on your listening, with songs to try." } },
            }
        }
    }
}

/// The picks found on YouTube Music, looked up once each.
fn use_found(ctx: Ctx, picks: Vec<Pick>) -> Vec<(Pick, Option<Option<Entry>>)> {
    let wanted = picks.clone();
    use_effect(use_reactive!(|wanted| {
        let Some(svc) = ctx.services() else { return };
        let mut found = ctx.insights.found;
        let missing: Vec<Pick> = wanted.iter().filter(|p| !found.peek().contains_key(&pick_key(p))).cloned().collect();
        if missing.is_empty() {
            return;
        }
        spawn(async move {
            for pick in missing {
                let key = pick_key(&pick);
                if found.peek().contains_key(&key) {
                    continue;
                }
                let query = format!("{} {}", pick.title, pick.artist);
                let entry = match svc.dl.search(&query, 5, SearchSource::MusicSongs).await {
                    Ok(entries) => best_match(&pick, entries),
                    Err(e) => {
                        // Offline, say: looked up again when the page next opens.
                        tracing::warn!(target: "ytmdl", "insights: looking up {query}: {e:#}");
                        return;
                    }
                };
                found.write().insert(key, entry);
            }
        });
    }));
    let found = (ctx.insights.found)();
    picks.into_iter().map(|p| {
        let f = found.get(&pick_key(&p)).cloned();
        (p, f)
    }).collect()
}

/// The result with the pick's title and artist, else by its artist, else
/// with its title, else the first.
fn best_match(pick: &Pick, entries: Vec<Entry>) -> Option<Entry> {
    let (title, artist) = (pick.title.to_lowercase(), pick.artist.to_lowercase());
    let by = |e: &Entry| e.artists.iter().any(|a| a.to_lowercase() == artist);
    let named = |e: &Entry| e.title.to_lowercase() == title;
    let i = entries
        .iter()
        .position(|e| by(e) && named(e))
        .or_else(|| entries.iter().position(by))
        .or_else(|| entries.iter().position(named))
        .unwrap_or(0);
    entries.into_iter().nth(i)
}

#[component]
pub(super) fn InsightsScreen() -> Element {
    let ctx = use_context::<Ctx>();
    let s = ctx.insights;
    let report = (s.report)();
    let pending = (s.pending)();
    let running = (s.running)();
    let error = (s.error)();
    let enabled = (s.settings)().enabled;
    // While a batch is out, it's checked on every minute this page is open.
    use_hook(move || {
        spawn(async move {
            loop {
                ctx.insights_tick_pending();
                let mut wait = document::eval("await new Promise(r => setTimeout(r, 60000)); dioxus.send(true);");
                if wait.recv::<bool>().await.is_err() {
                    return;
                }
            }
        })
    });
    let picks = report.as_ref().map(|r| r.songs.clone()).unwrap_or_default();
    let found = use_found(ctx, picks);
    let states = track_states(&ctx.queue.jobs().read(), &ctx.owned.read());
    let entries: Vec<Entry> = found.iter().filter_map(|(_, f)| f.clone().flatten()).collect();
    let rows: Vec<(Pick, Option<Option<(Entry, TrackState)>>)> = found
        .into_iter()
        .map(|(p, f)| {
            let f = f.map(|e| e.map(|e| with_states(vec![e], &states, &ctx.keys.read()).remove(0)));
            (p, f)
        })
        .collect();
    let busy = running.is_some() || pending.is_some();
    let status = match (&running, &pending) {
        (Some(text), _) => Some(text.clone()),
        (None, Some(p)) => Some(format!(
            "Sent to the Batch API {}; it's usually back within an hour.",
            super::sync::ago(insights::now_secs() - p.sent_at)
        )),
        _ => None,
    };
    let play_all = entries.clone();
    let save_all = entries.clone();
    rsx! {
        div { class: "insights",
            header { class: "topbar",
                div { class: "title-row",
                    button { class: "icon-btn", "aria-label": "Back", onclick: move |_| ctx.nav.back(),
                        Svg { icon: Icon::Back }
                    }
                    h1 { "Insights" }
                    if enabled {
                        button { class: "secondary small", disabled: busy, onclick: move |_| ctx.refresh_insights(),
                            Svg { icon: Icon::Retry, size: 18 }
                            "Refresh"
                        }
                    }
                }
                if let Some(r) = &report {
                    div { class: "sub",
                        {dotted([
                            Some(format!("From {} {}", insights::count_text(r.listens), if r.listens == 1 { "listen" } else { "listens" })),
                            Some(insights::day_text(r.made_at)),
                            Some(model_text(r.model, r.effort)),
                            Some(format!("{}{}", cost_text(r.cost), if r.batch { " (batch)" } else { "" })),
                        ])}
                    }
                }
            }
            if let Some(text) = status {
                div { class: "insights-status", div { class: "spinner small" } "{text}" }
            }
            if let Some(e) = error {
                div { class: "insights-status error", Svg { icon: Icon::Alert, size: 18 } "{e}" }
            }
            match report {
                None => rsx! {
                    div { class: "empty",
                        Svg { icon: Icon::Sparkles, size: 48 }
                        h2 { "No insights yet" }
                        p {
                            if !enabled {
                                "Turn on Listening insights in Settings."
                            } else if ctx.insights.key.read().is_none() {
                                "Add your Claude API key in Settings, then tap Refresh."
                            } else if busy {
                                "Claude is looking at your listening."
                            } else {
                                "Tap Refresh to have Claude look at your listening."
                            }
                        }
                    }
                },
                Some(r) => rsx! {
                    p { class: "insights-headline", "{r.headline}" }
                    section { class: "group",
                        h2 { "Your habits" }
                        div { class: "card",
                            for (i , o) in r.observations.iter().enumerate() {
                                div { key: "{i}", class: "fact",
                                    span { class: "f-ic", Svg { icon: kind_icon(&o.kind), size: 22 } }
                                    div {
                                        div { class: "title", "{o.title}" }
                                        div { class: "sub", "{o.body}" }
                                    }
                                }
                            }
                        }
                    }
                    section { class: "group",
                        h2 { "Songs for you" }
                        div { class: "card recs",
                            for (i , (pick , found)) in rows.into_iter().enumerate() {
                                PickRow { key: "{i}", pick, found }
                            }
                        }
                        if !entries.is_empty() {
                            div { class: "actions",
                                button { class: "secondary", onclick: move |_| ctx.play_entries(play_all.clone(), 0),
                                    {format!("Play all {}", play_all.len())}
                                }
                                button {
                                    class: "secondary",
                                    onclick: move |_| {
                                        let songs = Songs { tracks: Vec::new(), entries: save_all.clone() };
                                        ctx.nav.push(Overlay::Sheet(Sheet::Name { id: None, name: "Songs for you".into(), songs, role: None }));
                                    },
                                    "Save as playlist"
                                }
                            }
                        }
                    }
                },
            }
            if enabled && ctx.insights.key.read().is_some() {
                button { class: "ask-fab", onclick: move |_| ctx.nav.push(Overlay::Ask),
                    Svg { icon: Icon::Chat, size: 20 }
                    "Ask about your listening"
                }
            }
        }
    }
}

impl Ctx {
    fn insights_tick_pending(&self) {
        if self.insights.pending.peek().is_some() {
            self.poll_insights();
        }
    }
}

fn kind_icon(kind: &str) -> Icon {
    match kind {
        "time" => Icon::Moon,
        "skips" => Icon::Next,
        "loyalty" => Icon::Repeat,
        "discovery" => Icon::Radio,
        "place" => Icon::Volume,
        "mood" => Icon::Music,
        _ => Icon::Sparkles,
    }
}

/// A suggested song: playable once found on YouTube Music.
#[component]
fn PickRow(pick: Pick, found: Option<Option<(Entry, TrackState)>>) -> Element {
    let ctx = use_context::<Ctx>();
    let sub = format!("{} · {}", pick.artist, pick.why);
    match found {
        Some(Some((entry, state))) => {
            // Who it's by on YouTube Music, should that be another version.
            let sub = format!("{} · {}", entry.artists.join(", "), pick.why);
            let (play, more, dl) = (entry.clone(), entry.clone(), entry.clone());
            rsx! {
                div { class: "rec tappable", role: "button", onclick: move |_| ctx.play_entries(vec![play.clone()], 0),
                    Cover { url: entry.thumbnail.clone() }
                    div { class: "meta",
                        div { class: "title", "{entry.title}" }
                        div { class: "sub", "{sub}" }
                    }
                    if state.wants_download() {
                        button {
                            class: "icon-btn",
                            "aria-label": "Download",
                            onclick: move |e| {
                                e.stop_propagation();
                                ctx.download(dl.clone());
                            },
                            Svg { icon: Icon::DownloadCircle }
                        }
                    } else if state == TrackState::Done {
                        span { class: "icon-btn done", Svg { icon: Icon::CheckCircle } }
                    } else {
                        span { class: "icon-btn", div { class: "spinner small" } }
                    }
                    button {
                        class: "icon-btn",
                        "aria-label": "More",
                        onclick: move |e| {
                            e.stop_propagation();
                            ctx.entry_menu(more.clone());
                        },
                        Svg { icon: Icon::More }
                    }
                }
            }
        }
        other => rsx! {
            div { class: "rec",
                Cover { url: None }
                div { class: "meta",
                    div { class: "title", "{pick.title}" }
                    div { class: "sub",
                        if other.is_none() { "{sub}" } else { "{sub} · not found on YouTube Music" }
                    }
                }
            }
        },
    }
}

/// One turn of the chat.
#[derive(Clone, PartialEq)]
struct Bubble {
    me: bool,
    text: String,
    songs: Vec<Pick>,
    error: bool,
}

const SUGGESTIONS: [&str; 4] = [
    "What have I been playing most lately?",
    "Which songs do I always skip?",
    "Who have I stopped listening to?",
    "Find me something new for tonight",
];

/// A chat about the listening; nothing of it is kept when the page closes.
#[component]
pub(super) fn AskScreen() -> Element {
    let ctx = use_context::<Ctx>();
    let settings = ctx.insights.settings.peek().clone();
    let mut bubbles = use_signal(Vec::<Bubble>::new);
    let mut chat = use_signal(|| None::<Chat>);
    let mut thinking = use_signal(|| None::<String>);
    let mut draft = use_signal(String::new);
    let mut cost = use_signal(|| 0.0);

    let mut send = move |question: String| {
        let question = question.trim().to_string();
        if question.is_empty() || thinking.peek().is_some() {
            return;
        }
        let Some(claude) = ctx.claude() else {
            ctx.notify("Add your Claude API key in Settings first".into());
            return;
        };
        draft.set(String::new());
        bubbles.write().push(Bubble { me: true, text: question.clone(), songs: Vec::new(), error: false });
        thinking.set(Some("Thinking…".into()));
        let report = ctx.insights.report.peek().clone();
        spawn(async move {
            let _ = document::eval("setTimeout(() => document.querySelector('.msgs-end')?.scrollIntoView({behavior: 'smooth'}), 50)");
            let mut current = match chat.write().take() {
                Some(c) => c,
                None => {
                    ctx.library.import_listens().await;
                    match Chat::start(claude, report).await {
                        Ok(c) => c,
                        Err(e) => {
                            thinking.set(None);
                            bubbles.write().push(Bubble { me: false, text: format!("{e:#}"), songs: Vec::new(), error: true });
                            return;
                        }
                    }
                }
            };
            let mut queries = 0;
            let mut on_step = |step: Step| match step {
                Step::Query(sql) => {
                    tracing::debug!(target: "ytmdl", "insights query: {sql}");
                    queries += 1;
                    thinking.set(Some(format!("Looking into your history ({})…", plural(queries, "query", "queries"))));
                }
            };
            let reply = current.ask(&question, &mut on_step).await;
            cost.set(current.cost());
            chat.set(Some(current));
            thinking.set(None);
            bubbles.write().push(match reply {
                Ok(r) => Bubble { me: false, text: r.text, songs: r.songs, error: false },
                Err(e) => Bubble { me: false, text: format!("{e:#}"), songs: Vec::new(), error: true },
            });
            let _ = document::eval("setTimeout(() => document.querySelector('.msgs-end')?.scrollIntoView({behavior: 'smooth'}), 50)");
        });
    };

    let list = bubbles();
    let waiting = thinking();
    rsx! {
        div { class: "chatpage",
            header { class: "topbar",
                div { class: "title-row",
                    button { class: "icon-btn", "aria-label": "Back", onclick: move |_| ctx.nav.back(),
                        Svg { icon: Icon::Back }
                    }
                    h1 { "Ask" }
                    span { class: "model-pill",
                        {dotted([Some(model_text(settings.model, settings.effort)), (cost() > 0.0).then(|| cost_text(cost()))])}
                    }
                }
            }
            div { class: "msgs",
                if list.is_empty() {
                    div { class: "chat-intro",
                        Svg { icon: Icon::Sparkles, size: 32 }
                        p { "Ask Claude anything about your listening. It looks things up in your history to answer." }
                        p { class: "dim", "This chat isn't saved: it's gone when you leave this page." }
                    }
                }
                for (i , b) in list.into_iter().enumerate() {
                    ChatBubble { key: "{i}", bubble: b }
                }
                if let Some(text) = waiting {
                    div { class: "msg bot thinking", div { class: "spinner small" } "{text}" }
                }
                div { class: "msgs-end" }
            }
            if bubbles.read().is_empty() {
                div { class: "sugg",
                    for q in SUGGESTIONS {
                        button { key: "{q}", class: "chip", onclick: move |_| send(q.to_string()), "{q}" }
                    }
                }
            }
            form {
                class: "composer",
                onsubmit: move |e| {
                    e.prevent_default();
                    send(draft.peek().clone());
                },
                input {
                    class: "field",
                    r#type: "text",
                    placeholder: "Ask about your listening…",
                    value: draft(),
                    oninput: move |e| draft.set(e.value()),
                }
                button {
                    class: "send",
                    r#type: "submit",
                    "aria-label": "Send",
                    disabled: draft().trim().is_empty() || thinking().is_some(),
                    Svg { icon: Icon::Send, size: 20 }
                }
            }
        }
    }
}

#[component]
fn ChatBubble(bubble: Bubble) -> Element {
    let ctx = use_context::<Ctx>();
    let found = use_found(ctx, bubble.songs.clone());
    let entries: Vec<Entry> = found.iter().filter_map(|(_, f)| f.clone().flatten()).collect();
    let class = match (bubble.me, bubble.error) {
        (true, _) => "msg me",
        (false, true) => "msg bot error",
        (false, false) => "msg bot",
    };
    let paragraphs: Vec<String> = bubble.text.split("\n\n").map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect();
    let (play_all, save_all) = (entries.clone(), entries.clone());
    rsx! {
        div { class,
            for (i , p) in paragraphs.into_iter().enumerate() {
                p { key: "{i}", "{p}" }
            }
            for (i , (pick , f)) in found.into_iter().enumerate() {
                SongCard { key: "{i}", pick, found: f }
            }
            if entries.len() > 1 {
                div { class: "msg-actions",
                    button { class: "chip", onclick: move |_| ctx.play_entries(play_all.clone(), 0), {format!("Play all {}", play_all.len())} }
                    button {
                        class: "chip",
                        onclick: move |_| ctx.nav.push(Overlay::Sheet(Sheet::AddTo(Songs { tracks: Vec::new(), entries: save_all.clone() }))),
                        "Add to a playlist"
                    }
                }
            }
        }
    }
}

#[component]
fn SongCard(pick: Pick, found: Option<Option<Entry>>) -> Element {
    let ctx = use_context::<Ctx>();
    let sub = format!("{} · {}", pick.artist, pick.why);
    match found {
        Some(Some(entry)) => {
            let sub = format!("{} · {}", entry.artists.join(", "), pick.why);
            let play = entry.clone();
            rsx! {
                div { class: "songcard tappable", role: "button", onclick: move |_| ctx.play_entries(vec![play.clone()], 0),
                    Cover { url: entry.thumbnail.clone() }
                    div { class: "meta",
                        div { class: "title", "{entry.title}" }
                        div { class: "sub", "{sub}" }
                    }
                    span { class: "icon-btn", Svg { icon: Icon::Play, size: 20 } }
                }
            }
        }
        other => rsx! {
            div { class: "songcard",
                Cover { url: None }
                div { class: "meta",
                    div { class: "title", "{pick.title}" }
                    div { class: "sub",
                        if other.is_none() { "{sub}" } else { "{sub} · not found on YouTube Music" }
                    }
                }
            }
        },
    }
}

/// What songs played now are played from, for the listening log: the page
/// on top, or the tab.
pub(super) fn use_play_source(ctx: Ctx) {
    let from = use_memo(move || {
        let stack = ctx.nav.stack.read();
        let top = stack.iter().rev().map(|(_, o)| o).find(|o| !matches!(o, Overlay::Sheet(_) | Overlay::NowPlaying));
        match top {
            Some(Overlay::RemoteAlbum(open)) => {
                let kind = if open.radio.is_some() { "radio".to_string() } else { open.header.kind.clone().unwrap_or_else(|| "album".into()) };
                format!("{kind}: {}", open.header.title)
            }
            Some(Overlay::RemoteArtist(open)) => format!("artist: {}", open.name),
            Some(Overlay::RemoteList(open)) => format!("artist: {} ({})", open.artist, open.title),
            Some(Overlay::Album { title, .. }) => format!("album: {title}"),
            Some(Overlay::Artist(name)) => format!("artist: {name}"),
            Some(Overlay::Playlist(id)) => {
                let name = ctx.library.get().playlist(*id).ok().flatten().map(|p| p.name).unwrap_or_default();
                format!("playlist: {name}")
            }
            Some(Overlay::Stats) => "stats".into(),
            Some(Overlay::Insights) => "insights".into(),
            Some(Overlay::Ask) => "insights chat".into(),
            Some(Overlay::Duplicates | Overlay::Titles) => "library".into(),
            Some(_) => "other".into(),
            None => match (ctx.tab)() {
                Tab::Home => "home".into(),
                Tab::Library => "library".into(),
                Tab::Search => "search".into(),
                Tab::Downloads => "downloads".into(),
                Tab::Settings => "settings".into(),
            },
        }
    });
    use_effect(move || ctx.player.set_from(from()));
}
