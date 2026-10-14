//! Listening insights: Claude reads the listening history (a summary of it,
//! then its own read-only SQL over it, see crates/library/src/history.rs) and
//! says what stands out in it, with songs to try; and answers questions about
//! it in a chat that isn't kept.
//!
//! Requests go to the Claude API with the user's own key, through Python's
//! HTTPS (the downloader's runtime), so this needs the downloader up.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ytmdl_core::Runtime;
use ytmdl_library::{Library, VIEW_DOCS};

const API: &str = "https://api.anthropic.com/v1";
/// Requests may think for minutes at high effort.
const TIMEOUT_SECS: u64 = 600;
const MAX_TOKENS: u32 = 32_000;
/// Model turns in one run (each may run queries) before it is given up.
const MAX_TURNS: usize = 16;

/// Settings keys.
const SETTINGS: &str = "insights";
const KEY: &str = "anthropic_key";
const REPORT: &str = "insights_report";
const PENDING: &str = "insights_batch";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Model {
    Haiku,
    Sonnet,
    Opus,
}

impl Model {
    pub const ALL: [Model; 3] = [Model::Haiku, Model::Sonnet, Model::Opus];

    fn id(self) -> &'static str {
        match self {
            Model::Haiku => "claude-haiku-5-5",
            Model::Sonnet => "claude-sonnet-5-5",
            Model::Opus => "claude-opus-5-5",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Model::Haiku => "Haiku 5.5",
            Model::Sonnet => "Sonnet 5.5",
            Model::Opus => "Opus 5.5",
        }
    }

    pub fn note(self) -> &'static str {
        match self {
            Model::Haiku => "Fastest and cheapest; shallower reads",
            Model::Sonnet => "Recommended",
            Model::Opus => "Most thorough, about twice Sonnet's cost",
        }
    }

    /// US dollars per million input and output tokens.
    fn prices(self) -> (f64, f64) {
        match self {
            Model::Haiku => (0.10, 0.50),
            Model::Sonnet => (2.0, 10.0),
            Model::Opus => (4.0, 20.0),
        }
    }

    /// Whether a request the model declines is retried on another model by
    /// the API (Haiku has no fallback).
    fn fallbacks(self) -> bool {
        self != Model::Haiku
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    High,
    Max,
}

impl Effort {
    pub const ALL: [Effort; 4] = [Effort::Low, Effort::Medium, Effort::High, Effort::Max];

    fn id(self) -> &'static str {
        match self {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::Max => "max",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Effort::Low => "Low",
            Effort::Medium => "Medium",
            Effort::High => "High",
            Effort::Max => "Max",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Frequency {
    /// Only when Refresh is tapped.
    Manual,
    Daily,
    Weekly,
    Monthly,
}

impl Frequency {
    pub const ALL: [Frequency; 4] = [Frequency::Manual, Frequency::Daily, Frequency::Weekly, Frequency::Monthly];

    pub fn label(self) -> &'static str {
        match self {
            Frequency::Manual => "Manual",
            Frequency::Daily => "Daily",
            Frequency::Weekly => "Weekly",
            Frequency::Monthly => "Monthly",
        }
    }

    /// Seconds between runs; `None` for manual.
    pub fn every(self) -> Option<i64> {
        match self {
            Frequency::Manual => None,
            Frequency::Daily => Some(86_400),
            Frequency::Weekly => Some(7 * 86_400),
            Frequency::Monthly => Some(30 * 86_400),
        }
    }
}

/// The Settings section. The API key is kept apart, under [`KEY`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub enabled: bool,
    pub model: Model,
    pub effort: Effort,
    pub frequency: Frequency,
    pub wifi_only: bool,
    /// Reports through the Batch API: half price, from a fuller summary
    /// instead of the model's own queries, back within about an hour.
    pub batch: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { enabled: false, model: Model::Sonnet, effort: Effort::Medium, frequency: Frequency::Weekly, wifi_only: true, batch: true }
    }
}

impl Settings {
    pub fn load(library: &Library) -> Settings {
        library.setting(SETTINGS).ok().flatten().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    pub fn save(&self, library: &Library) -> ytmdl_library::Result<()> {
        library.set_setting(SETTINGS, &serde_json::to_string(self).expect("plain data"))
    }
}

pub fn api_key(library: &Library) -> Option<String> {
    library.setting(KEY).ok().flatten().filter(|k| !k.trim().is_empty())
}

pub fn set_api_key(library: &Library, key: &str) -> ytmdl_library::Result<()> {
    library.set_setting(KEY, &clean_key(key))
}

/// A pasted key without what pasting brings along: spaces and line breaks
/// from wrapping, invisible characters from rich text.
pub fn clean_key(key: &str) -> String {
    key.chars().filter(|c| c.is_ascii_graphic()).collect()
}

/// "sk-ant-…4f2a".
pub fn masked_key(key: &str) -> String {
    let tail: String = key.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
    let head: String = key.chars().take(7).collect();
    format!("{head}…{tail}")
}

/// What a run found.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Report {
    /// Unix seconds.
    pub made_at: i64,
    pub model: Model,
    pub effort: Effort,
    /// Whether it went through the Batch API (from a summary, at half price).
    #[serde(default)]
    pub batch: bool,
    /// Listens in the history then.
    pub listens: i64,
    pub headline: String,
    pub observations: Vec<Observation>,
    pub songs: Vec<Pick>,
    /// What the run cost, in US dollars (at list prices).
    pub cost: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Observation {
    pub title: String,
    pub body: String,
    /// time | skips | loyalty | discovery | place | mood | other: picks the icon.
    pub kind: String,
}

/// A song to try.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pick {
    pub title: String,
    pub artist: String,
    pub why: String,
}

impl Report {
    pub fn load(library: &Library) -> Option<Report> {
        library.setting(REPORT).ok().flatten().and_then(|s| serde_json::from_str(&s).ok())
    }

    pub fn save(&self, library: &Library) -> ytmdl_library::Result<()> {
        library.set_setting(REPORT, &serde_json::to_string(self).expect("plain data"))
    }
}

/// A report sent to the Batch API, not back yet.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pending {
    pub id: String,
    /// Unix seconds.
    pub sent_at: i64,
    pub model: Model,
    pub effort: Effort,
    pub listens: i64,
}

impl Pending {
    pub fn load(library: &Library) -> Option<Pending> {
        library.setting(PENDING).ok().flatten().and_then(|s| serde_json::from_str(&s).ok())
    }

    pub fn save(&self, library: &Library) -> ytmdl_library::Result<()> {
        library.set_setting(PENDING, &serde_json::to_string(self).expect("plain data"))
    }

    pub fn clear(library: &Library) -> ytmdl_library::Result<()> {
        library.set_setting(PENDING, "")
    }
}

/// Token counts of a run, for its cost.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Usage {
    input: u64,
    output: u64,
    cache_write: u64,
    cache_read: u64,
}

impl Usage {
    fn add(&mut self, usage: &Value) {
        let n = |k: &str| usage[k].as_u64().unwrap_or(0);
        self.input += n("input_tokens");
        self.output += n("output_tokens");
        self.cache_write += n("cache_creation_input_tokens");
        self.cache_read += n("cache_read_input_tokens");
    }

    /// In US dollars; batches cost half.
    fn cost(&self, model: Model, batch: bool) -> f64 {
        let (input, output) = model.prices();
        let read = if model == Model::Opus { 0.05 } else { 0.1 };
        let tokens_in = self.input as f64 + self.cache_write as f64 * 1.25 + self.cache_read as f64 * read;
        let cost = (tokens_in * input + self.output as f64 * output) / 1_000_000.0;
        if batch { cost / 2.0 } else { cost }
    }
}

/// One Claude API conversation's settings.
#[derive(Clone)]
pub struct Claude {
    pub rt: Runtime,
    pub library: Library,
    pub key: String,
    pub model: Model,
    pub effort: Effort,
}

/// What the model is doing, for the UI.
pub enum Step<'a> {
    /// Running a query over the history.
    Query(&'a str),
}

impl Claude {
    /// A request to the API at `path`; its answer as JSON, or what went wrong.
    async fn request(&self, method: &'static str, path: &str, body: Option<Value>, beta: Option<&str>) -> anyhow::Result<Value> {
        let mut headers = vec![
            ("x-api-key".to_string(), self.key.clone()),
            ("anthropic-version".to_string(), "2023-06-01".to_string()),
        ];
        if let Some(beta) = beta {
            headers.push(("anthropic-beta".to_string(), beta.to_string()));
        }
        let url = if path.starts_with("https://") { path.to_string() } else { format!("{API}{path}") };
        let (status, answer) = self.rt.http(method, url, headers, body.map(|b| b.to_string()), TIMEOUT_SECS).await?;
        if status == 200 {
            return Ok(match serde_json::from_slice(&answer) {
                Ok(json) => json,
                // Batch results are JSON lines.
                Err(_) => Value::String(String::from_utf8_lossy(&answer).into_owned()),
            });
        }
        let answer: Value = serde_json::from_slice(&answer).unwrap_or_default();
        let message = answer["error"]["message"].as_str().unwrap_or("no message");
        tracing::warn!(target: "ytmdl", "insights: {method} {path} answered {status}: {message}");
        anyhow::bail!(match status {
            401 if self.key.starts_with("sk-ant-oat") => {
                "That is a Claude sign-in token, not an API key. Create an API key at console.anthropic.com (API keys).".to_string()
            }
            401 => format!("The API key was refused ({message}). Check it in Settings."),
            403 => format!("The API key may not use this: {message}"),
            429 => format!("Too many requests for now; try again later. ({message})"),
            529 => "Claude is overloaded right now; try again in a while.".to_string(),
            _ => format!("The Claude API answered {status}: {message}"),
        })
    }

    /// Whether the key works: lists one model, which costs nothing.
    pub async fn check(&self) -> anyhow::Result<()> {
        self.request("GET", "/models?limit=1", None, None).await.map(drop)
    }

    /// A Messages request. The instructions and tools come first and don't
    /// change, so they are cached (`system`'s breakpoint) for every request
    /// with them; the conversation after them is cached up to its last message
    /// (the top-level `cache_control`), so each turn of a run or chat reads the
    /// ones before it from the cache.
    fn params(&self, system: &str, messages: &[Value], tools: &[Value], format: Option<&Value>) -> Value {
        let mut params = json!({
            "model": self.model.id(),
            "max_tokens": MAX_TOKENS,
            "system": [{ "type": "text", "text": system, "cache_control": { "type": "ephemeral" } }],
            "messages": messages,
            "cache_control": { "type": "ephemeral" },
            "output_config": { "effort": self.effort.id() },
        });
        if !tools.is_empty() {
            params["tools"] = json!(tools);
        }
        if let Some(format) = format {
            params["output_config"]["format"] = format.clone();
        }
        params
    }

    /// Sends one Messages request now. Sonnet and Opus may be answered by
    /// another model when they decline (not possible in batches).
    async fn send(&self, params: Value) -> anyhow::Result<Value> {
        let mut params = params;
        let beta = self.model.fallbacks().then(|| {
            params["fallbacks"] = json!("default");
            "server-side-fallback-2026-07-01"
        });
        self.request("POST", "/messages", Some(params), beta).await
    }

    /// Runs the conversation in `messages` until the model ends its turn,
    /// answering its tool calls: queries over the history, and `show_songs`
    /// (taken as shown). Appends every message.
    async fn run(
        &self,
        system: &str,
        messages: &mut Vec<Value>,
        tools: &[Value],
        format: Option<&Value>,
        usage: &mut Usage,
        on_step: &mut dyn FnMut(Step),
    ) -> anyhow::Result<()> {
        for _ in 0..MAX_TURNS {
            let answer = self.send(self.params(system, messages, tools, format)).await?;
            usage.add(&answer["usage"]);
            check_stop(&answer)?;
            let calls: Vec<Value> = answer["content"]
                .as_array()
                .map(|c| c.iter().filter(|b| b["type"] == "tool_use").cloned().collect())
                .unwrap_or_default();
            // Appended as it came, thinking included: the model's next turn needs it unchanged.
            messages.push(json!({ "role": "assistant", "content": answer["content"].clone() }));
            if answer["stop_reason"] != "tool_use" || calls.is_empty() {
                return Ok(());
            }
            let mut results = Vec::new();
            for call in calls {
                let (content, is_error) = match call["name"].as_str() {
                    Some("query_history") => {
                        let sql = call["input"]["sql"].as_str().unwrap_or_default().to_string();
                        on_step(Step::Query(&sql));
                        let library = self.library.clone();
                        match crate::blocking(move || library.query_history(&sql)).await? {
                            Ok(result) => (serde_json::to_string(&result)?, false),
                            Err(e) => (format!("The query failed: {e}"), true),
                        }
                    }
                    Some("show_songs") => ("Shown to the listener as songs they can tap to play.".to_string(), false),
                    _ => ("No such tool.".to_string(), true),
                };
                results.push(json!({
                    "type": "tool_result",
                    "tool_use_id": call["id"],
                    "content": content,
                    "is_error": is_error,
                }));
            }
            messages.push(json!({ "role": "user", "content": results }));
        }
        anyhow::bail!("Claude took too many steps without finishing.")
    }

    /// The first message of a report: the history's summary, and what the
    /// last report said, so it isn't said again.
    async fn report_prompt(&self, previous: Option<&Report>, detailed: bool) -> anyhow::Result<(String, i64)> {
        let library = self.library.clone();
        let (summary, info) =
            crate::blocking(move || anyhow::Ok((library.history_summary(detailed)?, library.history_info()?))).await??;
        let mut prompt = format!("Here is a summary of my listening history, as JSON tables (columns and rows):\n\n{summary}\n\n");
        prompt.push_str(if detailed {
            "Give me your report."
        } else {
            "Look into it, with queries where the summary isn't enough, and give me your report."
        });
        if let Some(p) = previous {
            let songs: Vec<String> = p.songs.iter().map(|s| format!("{} by {}", s.title, s.artist)).collect();
            let seen: Vec<&str> = p.observations.iter().map(|o| o.title.as_str()).collect();
            prompt.push_str(&format!(
                "\n\nYour last report ({}) said: {}. Its observations were titled: {}. It suggested: {}. \
                 Don't suggest those songs again, and only repeat an observation if something about it changed \
                 (then say what).",
                days_ago(p.made_at),
                p.headline,
                seen.join("; "),
                songs.join("; ")
            ));
        }
        Ok((prompt, info.listens))
    }

    /// A new report now, the model looking into the history with its own
    /// queries.
    pub async fn report(&self, previous: Option<&Report>, on_step: &mut dyn FnMut(Step)) -> anyhow::Result<Report> {
        let (prompt, listens) = self.report_prompt(previous, false).await?;
        let mut messages = vec![json!({ "role": "user", "content": prompt })];
        let mut usage = Usage::default();
        let system = system(REPORT_SYSTEM, QUERIES);
        self.run(&system, &mut messages, &[query_tool()], Some(&report_format()), &mut usage, on_step).await?;
        let found = found(messages.last().expect("the model's answer"))?;
        Ok(self.made(found, listens, false, usage))
    }

    /// Sends a report to the Batch API, from a fuller summary as the model
    /// can't query there; [`Claude::collect`] gets it when it's done.
    pub async fn submit(&self, previous: Option<&Report>) -> anyhow::Result<Pending> {
        let (prompt, listens) = self.report_prompt(previous, true).await?;
        let messages = [json!({ "role": "user", "content": prompt })];
        let mut params = self.params(&system(REPORT_SYSTEM, SUMMARY_ONLY), &messages, &[], Some(&report_format()));
        // One request: the summary after the instructions is never read back,
        // so caching it would only add the cost of writing it.
        params.as_object_mut().expect("an object").remove("cache_control");
        let batch = self.request("POST", "/messages/batches", Some(json!({
            "requests": [{ "custom_id": "report", "params": params }],
        })), None).await?;
        let id = batch["id"].as_str().ok_or_else(|| anyhow::anyhow!("the Batch API gave no batch id"))?;
        Ok(Pending { id: id.to_string(), sent_at: now_secs(), model: self.model, effort: self.effort, listens })
    }

    /// The batched report, once it's done (or why it failed); `None` while
    /// it's still being made. Fails only when the batch couldn't be asked about.
    pub async fn collect(&self, pending: &Pending) -> anyhow::Result<Option<anyhow::Result<Report>>> {
        let batch = self.request("GET", &format!("/messages/batches/{}", pending.id), None, None).await?;
        if batch["processing_status"] != "ended" {
            return Ok(None);
        }
        let Some(url) = batch["results_url"].as_str() else {
            return Ok(Some(Err(anyhow::anyhow!("The batch ended without results."))));
        };
        let results = self.request("GET", url, None, None).await?;
        let line = match &results {
            Value::String(lines) => lines.lines().find(|l| !l.trim().is_empty()).and_then(|l| serde_json::from_str(l).ok()),
            json => Some(json.clone()),
        }
        .unwrap_or(Value::Null);
        let result = &line["result"];
        Ok(Some(match result["type"].as_str() {
            Some("succeeded") => {
                let message = &result["message"];
                check_stop(message).and_then(|()| found(message)).map(|found| {
                    let mut usage = Usage::default();
                    usage.add(&message["usage"]);
                    let claude = Claude { model: pending.model, effort: pending.effort, ..self.clone() };
                    claude.made(found, pending.listens, true, usage)
                })
            }
            Some("errored") => Err(anyhow::anyhow!(
                "The batched report failed: {}",
                result["error"]["error"]["message"].as_str().unwrap_or("no reason given")
            )),
            Some("expired") => Err(anyhow::anyhow!("The batched report wasn't made within a day; try again.")),
            Some("canceled") => Err(anyhow::anyhow!("The batched report was canceled.")),
            _ => Err(anyhow::anyhow!("The batch's results couldn't be read.")),
        }))
    }

    fn made(&self, found: Found, listens: i64, batch: bool, usage: Usage) -> Report {
        Report {
            made_at: now_secs(),
            model: self.model,
            effort: self.effort,
            batch,
            listens,
            headline: found.headline,
            observations: found.observations,
            songs: found.songs,
            cost: usage.cost(self.model, batch),
        }
    }
}

/// Fails on answers that ended without one.
fn check_stop(message: &Value) -> anyhow::Result<()> {
    match message["stop_reason"].as_str() {
        Some("refusal") => anyhow::bail!("Claude declined to answer that."),
        Some("max_tokens") => anyhow::bail!("Claude's answer was too long and was cut off."),
        _ => Ok(()),
    }
}

/// The report as the model writes it.
#[derive(Deserialize)]
struct Found {
    headline: String,
    observations: Vec<Observation>,
    songs: Vec<Pick>,
}

/// The report in the assistant message `message`.
fn found(message: &Value) -> anyhow::Result<Found> {
    serde_json::from_str(&text(message)).map_err(|e| anyhow::anyhow!("Claude's report couldn't be read: {e}"))
}

/// A chat about the history, kept only while its page is open.
pub struct Chat {
    claude: Claude,
    system: String,
    /// The history's summary and the latest report, sent before the first question.
    context: String,
    messages: Vec<Value>,
    usage: Usage,
}

/// What the model said in one turn of a chat.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Reply {
    pub text: String,
    pub songs: Vec<Pick>,
}

impl Chat {
    /// Starts one with the history's summary (and the last report) to go on.
    pub async fn start(claude: Claude, report: Option<Report>) -> anyhow::Result<Chat> {
        let library = claude.library.clone();
        let summary = crate::blocking(move || library.history_summary(false)).await??;
        let mut context = format!("A summary of my listening history, as JSON tables:\n{summary}");
        if let Some(r) = report {
            context.push_str(&format!(
                "\n\nYour latest insights report, which I may ask about: {}",
                serde_json::to_string(&json!({
                    "made": days_ago(r.made_at),
                    "headline": r.headline,
                    "observations": r.observations,
                    "songs": r.songs,
                }))?
            ));
        }
        Ok(Chat { claude, system: system(CHAT_SYSTEM, ""), context, messages: Vec::new(), usage: Usage::default() })
    }

    /// Sends `question`; returns what the model said and showed since it.
    pub async fn ask(&mut self, question: &str, on_step: &mut dyn FnMut(Step)) -> anyhow::Result<Reply> {
        let start = self.messages.len();
        let content = if start == 0 {
            // The summary leads the conversation, so it is cached with it.
            json!([{ "type": "text", "text": self.context }, { "type": "text", "text": question }])
        } else {
            json!(question)
        };
        self.messages.push(json!({ "role": "user", "content": content }));
        let tools = [query_tool(), songs_tool()];
        let result = self.claude.run(&self.system, &mut self.messages, &tools, None, &mut self.usage, on_step).await;
        if let Err(e) = result {
            // The question goes, so the conversation stays valid to go on with.
            self.messages.truncate(start);
            return Err(e);
        }
        let mut reply = Reply::default();
        for message in self.messages[start..].iter().filter(|m| m["role"] == "assistant") {
            for block in message["content"].as_array().into_iter().flatten() {
                match block["type"].as_str() {
                    Some("text") => {
                        let text = block["text"].as_str().unwrap_or_default().trim();
                        if !text.is_empty() {
                            if !reply.text.is_empty() {
                                reply.text.push_str("\n\n");
                            }
                            reply.text.push_str(text);
                        }
                    }
                    Some("tool_use") if block["name"] == "show_songs" => {
                        if let Ok(songs) = serde_json::from_value::<Vec<Pick>>(block["input"]["songs"].clone()) {
                            reply.songs.extend(songs);
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(reply)
    }

    /// What the chat has cost so far, in US dollars.
    pub fn cost(&self) -> f64 {
        self.usage.cost(self.claude.model, false)
    }
}

/// The text of an assistant message.
fn text(message: &Value) -> String {
    message["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|b| b["type"] == "text")
        .filter_map(|b| b["text"].as_str())
        .collect::<Vec<_>>()
        .join("")
}

/// How a report run ended.
pub enum Outcome {
    Made(Report),
    /// Sent to the Batch API; [`poll`] gets it.
    Sent(Pending),
}

/// Makes a new report now, or sends it to the Batch API, with `settings`;
/// keeps what comes of it.
pub async fn refresh(rt: Runtime, library: Library, settings: &Settings, on_step: &mut dyn FnMut(Step)) -> anyhow::Result<Outcome> {
    let lib = library.clone();
    let (key, previous) = crate::blocking(move || (api_key(&lib), Report::load(&lib))).await?;
    let key = key.ok_or_else(|| anyhow::anyhow!("Add your Claude API key in Settings first."))?;
    let claude = Claude { rt, library: library.clone(), key, model: settings.model, effort: settings.effort };
    if settings.batch {
        let pending = claude.submit(previous.as_ref()).await?;
        tracing::info!(id = %pending.id, model = pending.model.id(), "insights: report sent as a batch");
        let saved = pending.clone();
        crate::blocking(move || saved.save(&library)).await??;
        return Ok(Outcome::Sent(pending));
    }
    let report = claude.report(previous.as_ref(), on_step).await?;
    tracing::info!(model = report.model.id(), cost = report.cost, "insights: report made");
    let saved = report.clone();
    crate::blocking(move || saved.save(&library)).await??;
    Ok(Outcome::Made(report))
}

/// Gets the batched report if it's done, and keeps it. A batch that failed, or
/// that is older than a day (they expire), is dropped.
pub async fn poll(rt: Runtime, library: Library) -> anyhow::Result<Option<Report>> {
    let lib = library.clone();
    let (key, pending) = crate::blocking(move || (api_key(&lib), Pending::load(&lib))).await?;
    let (Some(key), Some(pending)) = (key, pending) else { return Ok(None) };
    let claude = Claude { rt, library: library.clone(), key, model: pending.model, effort: pending.effort };
    // Not reached (offline, say): it's asked again later.
    let result = claude.collect(&pending).await?;
    let expired = now_secs() - pending.sent_at > 26 * 3600;
    match result {
        None if !expired => Ok(None),
        None => {
            crate::blocking(move || Pending::clear(&library)).await??;
            anyhow::bail!("The batched report wasn't made within a day; try again.")
        }
        Some(Ok(report)) => {
            tracing::info!(id = %pending.id, cost = report.cost, "insights: batched report back");
            let saved = report.clone();
            crate::blocking(move || {
                saved.save(&library)?;
                Pending::clear(&library)
            })
            .await??;
            Ok(Some(report))
        }
        Some(Err(e)) => {
            tracing::warn!(id = %pending.id, "insights: batch failed: {e:#}");
            crate::blocking(move || Pending::clear(&library)).await??;
            Err(e)
        }
    }
}

/// Whether a scheduled report is due at `now`.
pub fn due(settings: &Settings, last: Option<&Report>, pending: bool, now: i64) -> bool {
    match (settings.enabled, settings.frequency.every()) {
        (true, Some(every)) if !pending => last.is_none_or(|r| now - r.made_at >= every),
        _ => false,
    }
}

/// "3 days ago", for the model.
fn days_ago(secs: i64) -> String {
    match (now_secs() - secs) / 86_400 {
        0 => "today".into(),
        1 => "yesterday".into(),
        n => format!("{n} days ago"),
    }
}

/// Local time's offset from UTC, in seconds (from the library, which asks
/// SQLite: there's no time zone database here otherwise).
static UTC_OFFSET: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

pub fn set_utc_offset(secs: i64) {
    UTC_OFFSET.store(secs, std::sync::atomic::Ordering::Relaxed);
}

/// The local day number of unix time `secs`.
fn local_day(secs: i64) -> i64 {
    (secs + UTC_OFFSET.load(std::sync::atomic::Ordering::Relaxed)).div_euclid(86_400)
}

/// "today", "yesterday", "Tuesday" within a week, else "12 Oct".
pub fn day_text(secs: i64) -> String {
    let day = local_day(secs);
    match local_day(now_secs()) - day {
        ..=0 => "today".into(),
        1 => "yesterday".into(),
        2..=6 => ["Thursday", "Friday", "Saturday", "Sunday", "Monday", "Tuesday", "Wednesday"][day.rem_euclid(7) as usize].into(),
        _ => {
            let (_, m, d) = civil(day * 86_400);
            format!("{d} {}", MONTHS[m as usize - 1])
        }
    }
}

/// "Mar 2025".
pub fn month_year(secs: i64) -> String {
    let (y, m, _) = civil(local_day(secs) * 86_400);
    format!("{} {y}", MONTHS[m as usize - 1])
}

/// "4,812".
pub fn count_text(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 { format!("-{out}") } else { out }
}

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// The (UTC) year, month and day of unix time `secs`.
fn civil(secs: i64) -> (i64, u32, u32) {
    // Howard Hinnant's days-to-civil.
    let z = secs.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

pub fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

fn query_tool() -> Value {
    json!({
        "name": "query_history",
        "description": format!(
            "Runs one read-only SQLite SELECT over the listener's music history and returns up to 200 rows as \
             JSON (columns, rows, truncated). Use it to check a hunch or dig into something the summary only \
             hints at. Times are the listener's local time. The views:\n{VIEW_DOCS}"
        ),
        "input_schema": {
            "type": "object",
            "properties": { "sql": { "type": "string", "description": "One SELECT (or WITH ... SELECT) statement." } },
            "required": ["sql"],
            "additionalProperties": false,
        },
        "strict": true,
    })
}

fn songs_tool() -> Value {
    json!({
        "name": "show_songs",
        "description": "Shows songs to the listener as cards they can tap to play from YouTube Music, below your \
                        answer. Use it whenever you suggest or name songs for them to play.",
        "input_schema": {
            "type": "object",
            "properties": {
                "songs": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "title": { "type": "string" },
                            "artist": { "type": "string" },
                            "why": { "type": "string", "description": "A few words on why, tied to their listening." },
                        },
                        "required": ["title", "artist", "why"],
                        "additionalProperties": false,
                    },
                },
            },
            "required": ["songs"],
            "additionalProperties": false,
        },
        "strict": true,
    })
}

fn report_format() -> Value {
    json!({
        "type": "json_schema",
        "schema": {
            "type": "object",
            "properties": {
                "headline": { "type": "string" },
                "observations": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "title": { "type": "string" },
                            "body": { "type": "string" },
                            "kind": { "type": "string", "enum": ["time", "skips", "loyalty", "discovery", "place", "mood", "other"] },
                        },
                        "required": ["title", "body", "kind"],
                        "additionalProperties": false,
                    },
                },
                "songs": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "title": { "type": "string" },
                            "artist": { "type": "string" },
                            "why": { "type": "string" },
                        },
                        "required": ["title", "artist", "why"],
                        "additionalProperties": false,
                    },
                },
            },
            "required": ["headline", "observations", "songs"],
            "additionalProperties": false,
        },
    })
}

const ABOUT: &str = "\
The listener uses ytmdl, an Android music player that plays songs from YouTube Music, downloaded into their \
library or played without downloading. It logs every listen: when, how long it played, how it ended (finished, \
skipped to another song, replaced by another list, stopped), what it was played from (a playlist, album, artist, \
radio, search, Home, the queue, the car) and what it played through (speaker, headphones, a Bluetooth device by \
name, the car). Listens from before this was logged have only the time, song and how long it played. Plays \
count from 30 seconds (or half of a short song).";

const REPORT_SYSTEM: &str = "\
You look at one person's music listening history and tell them what is interesting about how they listen, and \
suggest songs for them.

{ABOUT}

{HOW}, and look for what they wouldn't have noticed themselves: when and where they listen, what they skip and when, \
what they come back to, what they have drifted from, how they find new music, how the car or the time of day \
changes what they play. Every observation rests on numbers you saw; give the numbers.

Write to them as \"you\", plainly and warmly, without flattery. The report:
- headline: the single most interesting thing, one sentence of under 30 words.
- observations: 5 to 7, the most surprising first. title under 6 words; body one or two sentences, under 45 \
words, with the numbers. kind picks its icon: time (when they listen), skips, loyalty (what they come back to), \
discovery (new music), place (where, or what it plays through), mood, other.
- songs: 10 to 12 songs that are on YouTube Music, not in their history: deeper cuts by artists they love and \
artists new to them close to what they play most, and a few that fit a habit you found. why: under 15 words, \
tied to their listening. Name real songs by their exact title and main artist.";

const CHAT_SYSTEM: &str = "\
You talk with one person about their music listening history: answer their questions about it, point out \
what is interesting in it, and suggest music.

{ABOUT}

Check what you say against the history with the query_history tool rather than guessing, and give the numbers. \
When you suggest or name songs to play, show them with the show_songs tool (real songs on YouTube Music, exact \
title and main artist), and keep the text about them short. This is a phone screen: answer in a few short \
paragraphs of plain text, no markdown (no headings, lists, tables or asterisks).";

/// How the model gets at the history, with the query tool.
const QUERIES: &str = "The summary you get is a start; check and dig with the query_history tool before you settle \
on anything";

/// ...and without it.
const SUMMARY_ONLY: &str = "Everything you know of the history is in the summary's tables; read them closely and \
across each other before you settle on anything";

/// The instructions, with [`ABOUT`] and `how` in place.
fn system(prompt: &str, how: &str) -> String {
    prompt.replace("{ABOUT}", ABOUT).replace("{HOW}", how)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(1_760_313_600), (2025, 10, 13));
        assert_eq!(month_year(1_709_251_200), "Mar 2024");
        assert_eq!(day_text(now_secs()), "today");
        assert_eq!(day_text(now_secs() - 86_400), "yesterday");
        assert_eq!((count_text(4812), count_text(999), count_text(1_234_567)), ("4,812".into(), "999".into(), "1,234,567".into()));
    }

    #[test]
    fn pasted_keys_lose_what_pasting_adds() {
        assert_eq!(clean_key(" sk-ant-api03-ab\ncd\u{200b}ef \n"), "sk-ant-api03-abcdef");
    }

    #[test]
    fn costs_count_the_cache_and_batches() {
        let mut usage = Usage::default();
        usage.add(&json!({ "input_tokens": 1_000_000, "output_tokens": 100_000, "cache_read_input_tokens": 1_000_000 }));
        assert!((usage.cost(Model::Sonnet, false) - (2.0 + 1.0 + 0.2)).abs() < 1e-9);
        assert!((usage.cost(Model::Sonnet, true) - 1.6).abs() < 1e-9);
        assert!((usage.cost(Model::Opus, false) - (4.0 + 2.0 + 0.2)).abs() < 1e-9);
    }

    #[test]
    fn reports_come_when_due() {
        let on = Settings { enabled: true, ..Settings::default() };
        let report = |made_at| Report {
            made_at,
            model: Model::Sonnet,
            effort: Effort::Medium,
            batch: false,
            listens: 1,
            headline: String::new(),
            observations: Vec::new(),
            songs: Vec::new(),
            cost: 0.0,
        };
        let now = 100 * 86_400;
        assert!(due(&on, None, false, now));
        assert!(!due(&on, None, true, now), "a batch is out");
        assert!(!due(&on, Some(&report(now - 86_400)), false, now));
        assert!(due(&on, Some(&report(now - 7 * 86_400)), false, now));
        assert!(!due(&Settings::default(), None, false, now), "off");
        assert!(!due(&Settings { frequency: Frequency::Manual, ..on }, None, false, now));
    }

    #[test]
    fn reads_reports_and_prompts() {
        let message = json!({ "content": [
            { "type": "thinking", "thinking": "", "signature": "x" },
            { "type": "text", "text": r#"{"headline": "h", "observations": [{"title": "t", "body": "b", "kind": "time"}], "songs": [{"title": "s", "artist": "a", "why": "w"}]}"# },
        ], "stop_reason": "end_turn" });
        check_stop(&message).unwrap();
        let found = found(&message).unwrap();
        assert_eq!((found.headline.as_str(), found.songs[0].artist.as_str()), ("h", "a"));
        assert!(check_stop(&json!({ "stop_reason": "refusal" })).is_err());

        let report = system(REPORT_SYSTEM, QUERIES);
        assert!(!report.contains('{') && report.contains("query_history") && report.contains("ytmdl"));
        assert!(system(REPORT_SYSTEM, SUMMARY_ONLY).contains("Everything you know"));
        assert!(!system(CHAT_SYSTEM, "").contains("{ABOUT}"));
        assert_eq!(masked_key("sk-ant-api03-abcdef1234"), "sk-ant-…1234");
    }
}
