//! Offline result-fusion comparison harness (Phase 4.3 experiment).
//!
//! Reads only fixtures that already exist in this repository and never performs
//! network I/O:
//!
//! * bench/quality-questions-100.json              (queries + gold URL lists)
//! * bench/content-{exa,tavily,searxng,rustxng}.json (recorded provider lists)
//! * bench/local-retrieval/development.json        (synthetic corpus + queries)
//! * bench/eval-100-final.json                     (recorded merged ranking)
//!
//! Existing production behaviour is untouched: this is an examples/ binary and no
//! library code is modified. Strategies under test:
//!
//! * existing          - the shipping ResultContainer::close fusion
//! * rrf60             - Reciprocal Rank Fusion, k = 60
//! * rrf60_weighted    - RRF with a fixed, a-priori source weight table
//! * normsum           - rank-normalised score sum
//! * base+diversity    - domain-diversity reordering on top of a base ranking
//! * solo:<source>     - each source alone (per-source reference)
//!
//! Metrics: Hit@1/5/10, MRR@10, nDCG@10 (binary gains), plus paired sign tests
//! against the shipping fusion.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::BufReader;
use std::path::{Path, PathBuf};

use rustxng::engine::EngineItem;
use rustxng::results::{ResultContainer, SearchResult, dedup_key, query_terms};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const DEFAULT_LIMIT: usize = 10;
const DEFAULT_RRF_K: f64 = 60.0;
const DEFAULT_LAMBDA: f64 = 0.5;
// Repository defaults for the general (multi-source) search path.
// src/settings.rs: relevance_weight = 4.0, authority_weight = 8.0.
const DEFAULT_RELEVANCE_WEIGHT: f64 = 4.0;
const DEFAULT_AUTHORITY_WEIGHT: f64 = 8.0;

// ---------------------------------------------------------------- CLI options

struct Options {
    datasets: Vec<String>,
    repo: PathBuf,
    out: Option<PathBuf>,
    limit: usize,
    rrf_k: f64,
    lambda: f64,
    weights: BTreeMap<String, f64>,
    relevance_weight: f64,
    authority_weight: f64,
    rows: bool,
}

impl Options {
    fn parse() -> Result<Self, String> {
        let mut options = Self {
            datasets: vec![
                "providers3".to_string(),
                "dev40".to_string(),
                "recorded100".to_string(),
            ],
            repo: repo_root(),
            out: None,
            limit: DEFAULT_LIMIT,
            rrf_k: DEFAULT_RRF_K,
            lambda: DEFAULT_LAMBDA,
            weights: default_weights(),
            relevance_weight: DEFAULT_RELEVANCE_WEIGHT,
            authority_weight: DEFAULT_AUTHORITY_WEIGHT,
            rows: false,
        };
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            let value = args
                .next()
                .ok_or_else(|| format!("missing value for {arg}"))?;
            match arg.as_str() {
                "--dataset" => {
                    options.datasets = value
                        .split(',')
                        .map(|item| item.trim().to_string())
                        .filter(|item| !item.is_empty())
                        .collect();
                }
                "--repo" => options.repo = value.into(),
                "--out" => options.out = Some(value.into()),
                "--limit" => {
                    options.limit = value.parse().map_err(|_| "invalid limit".to_string())?
                }
                "--rrf-k" => options.rrf_k = value.parse().map_err(|_| "invalid rrf-k")?,
                "--lambda" => options.lambda = value.parse().map_err(|_| "invalid lambda")?,
                "--weights" => options.weights = parse_weights(&value)?,
                "--relevance-weight" => {
                    options.relevance_weight = value
                        .parse()
                        .map_err(|_| "invalid relevance-weight".to_string())?
                }
                "--authority-weight" => {
                    options.authority_weight = value
                        .parse()
                        .map_err(|_| "invalid authority-weight".to_string())?
                }
                "--rows" => options.rows = value != "false",
                other => return Err(format!("unknown option {other}")),
            }
        }
        if options.limit == 0 {
            return Err("limit must be positive".to_string());
        }
        Ok(options)
    }
}

// A-priori source weights for weighted RRF.
//
// Declared before any measurement, from recorded output shape only: searxng and
// rustxng are themselves merged multi-engine lists (up to ~50 URLs for searxng),
// so their tail is noisier and double counts engine agreement; the two
// single-API providers keep full weight. These are not fitted to the test set.
fn default_weights() -> BTreeMap<String, f64> {
    [
        ("exa", 1.0),
        ("tavily", 1.0),
        ("searxng", 0.6),
        ("rustxng", 0.6),
    ]
    .into_iter()
    .map(|(name, weight)| (name.to_string(), weight))
    .collect()
}

fn parse_weights(spec: &str) -> Result<BTreeMap<String, f64>, String> {
    let mut map = BTreeMap::new();
    for part in spec.split(',') {
        let (name, value) = part
            .split_once('=')
            .ok_or_else(|| format!("invalid weight {part}"))?;
        let parsed = value
            .parse::<f64>()
            .map_err(|_| format!("invalid weight {part}"))?;
        map.insert(name.trim().to_string(), parsed);
    }
    Ok(map)
}

// ------------------------------------------------------------- fixture layout

#[derive(Deserialize)]
struct QuestionsFile {
    queries: Vec<Question>,
}

#[derive(Deserialize)]
struct Question {
    id: String,
    query: String,
    #[serde(default)]
    gold: Vec<String>,
    #[serde(default)]
    lang: Option<String>,
    #[serde(default)]
    category: Option<String>,
}

#[derive(Deserialize)]
struct RecordedFile {
    #[allow(dead_code)]
    source: String,
    queries: Vec<RecordedQuery>,
}

#[derive(Deserialize)]
struct RecordedQuery {
    id: String,
    #[serde(default)]
    results: Vec<RecordedItem>,
}

#[derive(Clone, Deserialize)]
struct RecordedItem {
    url: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    content: String,
}

#[derive(Deserialize)]
struct DevFixture {
    documents: Vec<DevDocument>,
    queries: Vec<DevQuery>,
}

#[derive(Deserialize)]
struct DevDocument {
    url: String,
    title: String,
    body: String,
    #[allow(dead_code)]
    lang: String,
}

#[derive(Deserialize)]
struct DevQuery {
    id: String,
    query: String,
    gold_urls: Vec<String>,
}

#[derive(Deserialize)]
struct Recorded100File {
    rows: Vec<Recorded100Row>,
}

#[derive(Deserialize)]
struct Recorded100Row {
    id: String,
    #[serde(default)]
    urls: Vec<String>,
}

// One ranked list produced by one source for one query.
struct SourceList {
    name: String,
    weight: f64,
    items: Vec<Doc>,
}

struct Doc {
    url: String,
    title: String,
    content: String,
}

struct Case {
    id: String,
    query: String,
    gold: Vec<String>,
    #[allow(dead_code)]
    lang: String,
    #[allow(dead_code)]
    category: String,
    sources: Vec<SourceList>,
}

// ------------------------------------------------------------------- utilities

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, String> {
    let file = std::fs::File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    serde_json::from_reader(BufReader::new(file))
        .map_err(|error| format!("{}: {error}", path.display()))
}

fn sha256(path: &Path) -> String {
    let Ok(bytes) = std::fs::read(path) else {
        return String::new();
    };
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn host_of(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_lowercase))
        .unwrap_or_default()
}

// ------------------------------------------------------------- gold matching

// Canonical key comparable across providers (dedup_key of the shipping code).
fn canon(url: &str) -> String {
    dedup_key(url)
}

fn split_key(key: &str) -> (&str, &str) {
    match key.find('/') {
        Some(index) => (&key[..index], &key[index..]),
        None => (key, ""),
    }
}

// Gold is a URL prefix: the gold page, or a deeper page under it.
fn is_relevant(candidate_key: &str, gold_keys: &[&str]) -> bool {
    let (candidate_host, candidate_rest) = split_key(candidate_key);
    gold_keys.iter().any(|gold| {
        let (gold_host, gold_rest) = split_key(gold);
        if candidate_host != gold_host {
            return false;
        }
        if gold_rest.is_empty() {
            return true;
        }
        match candidate_rest.strip_prefix(gold_rest) {
            Some("") => true,
            Some(rest) => rest.starts_with('/') || rest.starts_with('.') || rest.starts_with('?'),
            None => false,
        }
    })
}

// -------------------------------------------------------------------- metrics

#[derive(Default, Clone, Copy)]
struct Metrics {
    hit1: f64,
    hit5: f64,
    hit10: f64,
    mrr10: f64,
    ndcg10: f64,
}

struct Observation {
    // 1-based rank of the first relevant result within the cutoff, 0 = miss.
    rank: usize,
    mrr: f64,
    ndcg: f64,
}

fn observe(ranked: &[String], gold: &[String], limit: usize) -> Observation {
    let gold_keys: Vec<String> = gold.iter().map(|url| canon(url)).collect();
    let gold_keys: Vec<&str> = gold_keys.iter().map(String::as_str).collect();

    let mut rank = 0usize;
    let mut dcg = 0.0;
    let mut relevant_found = 0usize;
    let depth = ranked.len().min(limit);
    for (index, url) in ranked.iter().take(depth).enumerate() {
        let position = index + 1;
        if is_relevant(&canon(url), &gold_keys) {
            if rank == 0 {
                rank = position;
            }
            relevant_found += 1;
            dcg += 1.0 / (position as f64 + 1.0).log2();
        }
    }
    let ideal_depth = relevant_found.min(limit);
    let mut idcg = 0.0;
    for position in 1..=ideal_depth {
        idcg += 1.0 / (position as f64 + 1.0).log2();
    }
    let ndcg = if idcg > 0.0 { dcg / idcg } else { 0.0 };
    let mrr = if rank > 0 && rank <= limit {
        1.0 / rank as f64
    } else {
        0.0
    };
    Observation { rank, mrr, ndcg }
}

fn aggregate(observations: &[Observation]) -> Metrics {
    let n = observations.len().max(1) as f64;
    Metrics {
        hit1: observations.iter().filter(|o| o.rank == 1).count() as f64 / n,
        hit5: observations
            .iter()
            .filter(|o| o.rank >= 1 && o.rank <= 5)
            .count() as f64
            / n,
        hit10: observations
            .iter()
            .filter(|o| o.rank >= 1 && o.rank <= 10)
            .count() as f64
            / n,
        mrr10: observations.iter().map(|o| o.mrr).sum::<f64>() / n,
        ndcg10: observations.iter().map(|o| o.ndcg).sum::<f64>() / n,
    }
}

// Two-sided exact sign test over per-query paired differences.
fn sign_test(wins: usize, losses: usize) -> f64 {
    let n = wins + losses;
    if n == 0 {
        return 1.0;
    }
    let tail = wins.min(losses);
    let mut coefficient = 1.0f64;
    let mut sum = 1.0f64;
    for index in 1..=tail {
        coefficient *= (n - index + 1) as f64 / index as f64;
        sum += coefficient;
    }
    (2.0 * sum / 2f64.powi(n as i32)).min(1.0)
}

// ------------------------------------------------------------------ fusion core

fn rrf(lists: &[SourceList], k: f64, weighted: bool, limit: usize) -> Vec<(String, f64)> {
    let mut scores: HashMap<String, f64> = HashMap::new();
    let mut first_seen: HashMap<String, usize> = HashMap::new();
    let mut representative: HashMap<String, String> = HashMap::new();
    let mut sequence = 0usize;
    for list in lists {
        let weight = if weighted { list.weight } else { 1.0 };
        if weight <= 0.0 {
            continue;
        }
        let mut seen_here: BTreeSet<String> = BTreeSet::new();
        let mut position = 0usize;
        for item in &list.items {
            let key = canon(&item.url);
            if !seen_here.insert(key.clone()) {
                continue;
            }
            position += 1;
            *scores.entry(key.clone()).or_insert(0.0) += weight / (k + position as f64);
            first_seen.entry(key.clone()).or_insert_with(|| {
                let index = sequence;
                sequence += 1;
                index
            });
            representative
                .entry(key)
                .or_insert_with(|| item.url.clone());
        }
    }
    let mut fused: Vec<(String, f64, usize)> = scores
        .into_iter()
        .map(|(key, score)| {
            let order = first_seen[&key];
            (representative[&key].clone(), score, order)
        })
        .collect();
    fused.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.2.cmp(&b.2))
    });
    fused.truncate(limit);
    fused
        .into_iter()
        .map(|(url, score, _)| (url, score))
        .collect()
}

// Rank-normalised score sum: each list contributes (n - rank) / (n - 1).
fn normsum(lists: &[SourceList], limit: usize) -> Vec<(String, f64)> {
    let mut scores: HashMap<String, f64> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut representative: HashMap<String, String> = HashMap::new();
    for list in lists {
        let length = list.items.len();
        if length == 0 {
            continue;
        }
        let mut seen_here: BTreeSet<String> = BTreeSet::new();
        for (index, item) in list.items.iter().enumerate() {
            let key = canon(&item.url);
            if !seen_here.insert(key.clone()) {
                continue;
            }
            let normalised = if length == 1 {
                1.0
            } else {
                (length - index - 1) as f64 / (length - 1) as f64
            };
            if !scores.contains_key(&key) {
                order.push(key.clone());
                representative.insert(key.clone(), item.url.clone());
            }
            *scores.entry(key).or_insert(0.0) += normalised;
        }
    }
    let mut ranked: Vec<(String, f64)> = order
        .iter()
        .map(|key| (representative[key].clone(), scores[key]))
        .collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    ranked.truncate(limit);
    ranked
}

// Greedy domain-diversity reordering on a base ranking: pick the next result
// maximising score / (1 + lambda * picks_from_that_host); ties keep base order.
fn diversify(base: &[(String, f64)], lambda: f64, limit: usize) -> Vec<(String, f64)> {
    if lambda <= 0.0 {
        return base.iter().take(limit).cloned().collect();
    }
    let mut remaining: Vec<(String, f64)> = base.to_vec();
    let mut picked: Vec<(String, f64)> = Vec::with_capacity(remaining.len().min(limit));
    let mut host_counts: HashMap<String, usize> = HashMap::new();
    while !remaining.is_empty() && picked.len() < limit {
        let mut best = 0usize;
        let mut best_effective = f64::NEG_INFINITY;
        for (index, (url, score)) in remaining.iter().enumerate() {
            let picks = host_counts.get(&host_of(url)).copied().unwrap_or(0);
            let effective = score / (1.0 + lambda * picks as f64);
            if effective > best_effective {
                best_effective = effective;
                best = index;
            }
        }
        let (url, score) = remaining.remove(best);
        *host_counts.entry(host_of(&url)).or_insert(0) += 1;
        picked.push((url, score));
    }
    picked
}

// The shipping fusion: ResultContainer::extend per source, then close.
fn existing(
    question: &str,
    lists: &[SourceList],
    limit: usize,
    relevance_weight: f64,
    authority_weight: f64,
) -> Vec<(String, f64)> {
    let mut container = ResultContainer::new();
    for list in lists {
        let items: Vec<EngineItem> = list
            .items
            .iter()
            .map(|doc| {
                let mut result = SearchResult::new(doc.url.clone(), doc.title.clone());
                result.content = doc.content.clone();
                EngineItem::Result(result)
            })
            .collect();
        container.extend(&list.name, list.weight, items);
    }
    let terms = query_terms(question);
    container.close(
        |_| 1.0,
        &terms,
        relevance_weight,
        question,
        authority_weight,
    );
    container
        .ordered()
        .iter()
        .take(limit)
        .map(|result| (result.url.clone(), result.score))
        .collect()
}

// --------------------------------------------------- synthetic dev40 rankers

fn tokenize(text: &str) -> Vec<String> {
    let lower = text.to_lowercase();
    let mut tokens: Vec<String> = lower
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| word.chars().count() >= 2)
        .map(str::to_string)
        .collect();
    let cjk: Vec<char> = lower
        .chars()
        .filter(|character| !character.is_ascii() && character.is_alphanumeric())
        .collect();
    for window in cjk.windows(2) {
        tokens.push(window.iter().collect());
    }
    tokens
}

fn bm25(
    documents: &[DevDocument],
    field_title: f64,
    field_body: f64,
    query: &str,
    limit: usize,
) -> Vec<Doc> {
    const K1: f64 = 1.2;
    const B: f64 = 0.75;
    let terms = tokenize(query);
    if terms.is_empty() || documents.is_empty() {
        return Vec::new();
    }
    let titles: Vec<Vec<String>> = documents.iter().map(|doc| tokenize(&doc.title)).collect();
    let bodies: Vec<Vec<String>> = documents.iter().map(|doc| tokenize(&doc.body)).collect();
    let average = |sets: &[Vec<String>]| {
        sets.iter().map(|set| set.len() as f64).sum::<f64>() / sets.len().max(1) as f64
    };
    let title_average = average(&titles).max(1.0);
    let body_average = average(&bodies).max(1.0);
    let document_frequency = |term: &str| {
        documents
            .iter()
            .enumerate()
            .filter(|(index, _)| {
                titles[*index].iter().any(|token| token == term)
                    || bodies[*index].iter().any(|token| token == term)
            })
            .count()
    };
    let total = documents.len() as f64;
    let idf: HashMap<&str, f64> = terms
        .iter()
        .map(|term| {
            let df = document_frequency(term) as f64;
            let value = ((total - df + 0.5) / (df + 0.5) + 1.0).ln();
            (term.as_str(), value.max(0.0))
        })
        .collect();
    let mut scored: Vec<(usize, f64)> = Vec::new();
    for (index, _) in documents.iter().enumerate() {
        let mut score = 0.0;
        for term in &terms {
            let weight = *idf.get(term.as_str()).unwrap_or(&0.0);
            if weight == 0.0 {
                continue;
            }
            let term_score = |set: &[String], average_length: f64, field_weight: f64| {
                if field_weight == 0.0 {
                    return 0.0;
                }
                let raw = set.iter().filter(|token| *token == term).count() as f64;
                if raw == 0.0 {
                    return 0.0;
                }
                let length = set.len() as f64;
                field_weight * weight * (raw * (K1 + 1.0))
                    / (raw + K1 * (1.0 - B + B * length / average_length))
            };
            score += term_score(&titles[index], title_average, field_title);
            score += term_score(&bodies[index], body_average, field_body);
        }
        if score > 0.0 {
            scored.push((index, score));
        }
    }
    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(&b.0))
    });
    scored
        .into_iter()
        .take(limit)
        .map(|(index, _)| Doc {
            url: documents[index].url.clone(),
            title: documents[index].title.clone(),
            content: documents[index].body.clone(),
        })
        .collect()
}

fn char_bigrams(text: &str) -> BTreeSet<String> {
    let chars: Vec<char> = text.to_lowercase().chars().collect();
    chars
        .windows(2)
        .map(|window| window.iter().collect())
        .collect()
}

// Character-bigram cosine: a similarity family that tolerates morphology
// differences (the Japanese and Chinese queries).
fn bigram_ranker(documents: &[DevDocument], query: &str, limit: usize) -> Vec<Doc> {
    let query_grams = char_bigrams(query);
    if query_grams.is_empty() {
        return Vec::new();
    }
    let mut scored: Vec<(usize, f64)> = Vec::new();
    for (index, document) in documents.iter().enumerate() {
        let grams = char_bigrams(&format!("{} {}", document.title, document.body));
        let overlap = query_grams.intersection(&grams).count() as f64;
        if overlap == 0.0 {
            continue;
        }
        let score = overlap / (query_grams.len() as f64 * grams.len().max(1) as f64).sqrt();
        scored.push((index, score));
    }
    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(&b.0))
    });
    scored
        .into_iter()
        .take(limit)
        .map(|(index, _)| Doc {
            url: documents[index].url.clone(),
            title: documents[index].title.clone(),
            content: documents[index].body.clone(),
        })
        .collect()
}

// -------------------------------------------------------------- dataset loaders

struct Dataset {
    name: String,
    cases: Vec<Case>,
    sources: Vec<String>,
    fixtures: Vec<(String, String)>,
}

fn load_questions(root: &Path) -> Result<Vec<Question>, String> {
    let file: QuestionsFile = read_json(&root.join("bench/quality-questions-100.json"))?;
    Ok(file.queries)
}

fn load_recorded(root: &Path, source: &str) -> Result<RecordedFile, String> {
    read_json(&root.join(format!("bench/content-{source}.json")))
}

fn provider_dataset(
    root: &Path,
    name: &str,
    sources: &[&str],
    questions: &[Question],
) -> Result<Dataset, String> {
    let mut per_source: Vec<(String, RecordedFile)> = Vec::new();
    for source in sources {
        per_source.push(((*source).to_string(), load_recorded(root, source)?));
    }
    let index: Vec<HashMap<String, Vec<RecordedItem>>> = per_source
        .iter()
        .map(|(_, file)| {
            file.queries
                .iter()
                .map(|query| (query.id.clone(), query.results.clone()))
                .collect()
        })
        .collect();

    let gold_by_id: HashMap<String, Vec<String>> = questions
        .iter()
        .map(|question| (question.id.clone(), question.gold.clone()))
        .collect();
    let meta: HashMap<String, (String, String, String)> = questions
        .iter()
        .map(|question| {
            (
                question.id.clone(),
                (
                    question.query.clone(),
                    question.lang.clone().unwrap_or_default(),
                    question.category.clone().unwrap_or_default(),
                ),
            )
        })
        .collect();

    let weights = default_weights();
    let mut cases = Vec::new();
    for question in questions {
        if index.iter().any(|map| {
            map.get(&question.id)
                .map(|items| items.is_empty())
                .unwrap_or(true)
        }) {
            continue;
        }
        let mut lists = Vec::new();
        for (position, (source, _)) in per_source.iter().enumerate() {
            let items = index[position]
                .get(&question.id)
                .cloned()
                .unwrap_or_default();
            if items.is_empty() {
                continue;
            }
            lists.push(SourceList {
                name: source.clone(),
                weight: weights.get(source).copied().unwrap_or(1.0),
                items: items
                    .into_iter()
                    .map(|item| Doc {
                        url: item.url,
                        title: item.title,
                        content: item.content,
                    })
                    .collect(),
            });
        }
        let (query, lang, category) = meta
            .get(&question.id)
            .cloned()
            .unwrap_or_else(|| (question.query.clone(), String::new(), String::new()));
        cases.push(Case {
            id: question.id.clone(),
            query,
            gold: gold_by_id.get(&question.id).cloned().unwrap_or_default(),
            lang,
            category,
            sources: lists,
        });
    }
    let fixtures = sources
        .iter()
        .map(|source| {
            let path = root.join(format!("bench/content-{source}.json"));
            (format!("bench/content-{source}.json"), sha256(&path))
        })
        .collect();
    Ok(Dataset {
        name: name.to_string(),
        cases,
        sources: sources.iter().map(|source| source.to_string()).collect(),
        fixtures,
    })
}

fn dev40_dataset(root: &Path, limit: usize) -> Result<Dataset, String> {
    let path = root.join("bench/local-retrieval/development.json");
    let fixture: DevFixture = read_json(&path)?;
    let mut cases = Vec::new();
    for query in &fixture.queries {
        let title = bm25(&fixture.documents, 1.0, 0.0, &query.query, limit);
        let body = bm25(&fixture.documents, 0.0, 1.0, &query.query, limit);
        let both = bm25(&fixture.documents, 2.0, 1.0, &query.query, limit);
        let grams = bigram_ranker(&fixture.documents, &query.query, limit);
        cases.push(Case {
            id: query.id.clone(),
            query: query.query.clone(),
            gold: query.gold_urls.clone(),
            lang: String::new(),
            category: String::new(),
            sources: vec![
                SourceList {
                    name: "title".to_string(),
                    weight: 1.0,
                    items: title,
                },
                SourceList {
                    name: "body".to_string(),
                    weight: 1.0,
                    items: body,
                },
                SourceList {
                    name: "both".to_string(),
                    weight: 1.0,
                    items: both,
                },
                SourceList {
                    name: "bigram".to_string(),
                    weight: 1.0,
                    items: grams,
                },
            ],
        });
    }
    Ok(Dataset {
        name: "dev40".to_string(),
        cases,
        sources: vec![
            "title".to_string(),
            "body".to_string(),
            "both".to_string(),
            "bigram".to_string(),
        ],
        fixtures: vec![(
            "bench/local-retrieval/development.json".to_string(),
            sha256(&path),
        )],
    })
}

fn recorded100_dataset(root: &Path, questions: &[Question]) -> Result<Dataset, String> {
    let path = root.join("bench/eval-100-final.json");
    let file: Recorded100File = read_json(&path)?;
    let gold_by_id: HashMap<String, Vec<String>> = questions
        .iter()
        .map(|question| (question.id.clone(), question.gold.clone()))
        .collect();
    let meta: HashMap<String, (String, String)> = questions
        .iter()
        .map(|question| {
            (
                question.id.clone(),
                (
                    question.query.clone(),
                    question.lang.clone().unwrap_or_default(),
                ),
            )
        })
        .collect();
    let mut cases = Vec::new();
    for row in &file.rows {
        if row.urls.is_empty() {
            continue;
        }
        let (query, lang) = meta
            .get(&row.id)
            .cloned()
            .unwrap_or_else(|| (row.id.clone(), String::new()));
        cases.push(Case {
            id: row.id.clone(),
            query,
            gold: gold_by_id.get(&row.id).cloned().unwrap_or_default(),
            lang,
            category: String::new(),
            sources: vec![SourceList {
                name: "recorded".to_string(),
                weight: 1.0,
                items: row
                    .urls
                    .iter()
                    .map(|url| Doc {
                        url: url.clone(),
                        title: String::new(),
                        content: String::new(),
                    })
                    .collect(),
            }],
        });
    }
    Ok(Dataset {
        name: "recorded100".to_string(),
        cases,
        sources: vec!["recorded".to_string()],
        fixtures: vec![
            ("bench/eval-100-final.json".to_string(), sha256(&path)),
            (
                "bench/quality-questions-100.json".to_string(),
                sha256(&root.join("bench/quality-questions-100.json")),
            ),
        ],
    })
}

// --------------------------------------------------------------- measurement

type RankingFn = Box<dyn Fn(&Case) -> Vec<(String, f64)>>;

struct StrategyOutcome {
    name: String,
    observations: Vec<Observation>,
    metrics: Metrics,
    ranks: Vec<usize>,
}

fn measure(dataset: &Dataset, options: &Options) -> Value {
    let limit = options.limit;
    let lambda = options.lambda;
    let rrf_k = options.rrf_k;
    let relevance_weight = options.relevance_weight;
    let authority_weight = options.authority_weight;

    let mut definitions: Vec<(&'static str, RankingFn)> = Vec::new();
    definitions.push((
        "existing",
        Box::new(move |case: &Case| {
            existing(
                &case.query,
                &case.sources,
                limit,
                relevance_weight,
                authority_weight,
            )
        }),
    ));
    definitions.push((
        "rrf60",
        Box::new(move |case: &Case| rrf(&case.sources, rrf_k, false, limit)),
    ));
    definitions.push((
        "rrf60_weighted",
        Box::new(move |case: &Case| rrf(&case.sources, rrf_k, true, limit)),
    ));
    definitions.push((
        "normsum",
        Box::new(move |case: &Case| normsum(&case.sources, limit)),
    ));
    definitions.push((
        "existing+diversity",
        Box::new(move |case: &Case| {
            diversify(
                &existing(
                    &case.query,
                    &case.sources,
                    limit,
                    relevance_weight,
                    authority_weight,
                ),
                lambda,
                limit,
            )
        }),
    ));
    definitions.push((
        "rrf60+diversity",
        Box::new(move |case: &Case| {
            diversify(&rrf(&case.sources, rrf_k, false, limit), lambda, limit)
        }),
    ));
    for source in &dataset.sources {
        let source_name = source.clone();
        let label: &'static str = Box::leak(format!("solo:{source_name}").into_boxed_str());
        definitions.push((
            label,
            Box::new(move |case: &Case| {
                case.sources
                    .iter()
                    .find(|list| list.name == source_name)
                    .map(|list| {
                        list.items
                            .iter()
                            .take(limit)
                            .map(|doc| (doc.url.clone(), 0.0))
                            .collect()
                    })
                    .unwrap_or_default()
            }),
        ));
    }

    let mut strategies: Vec<StrategyOutcome> = Vec::new();
    let mut per_query_rows = Vec::new();
    for (name, strategy) in &definitions {
        let mut observations = Vec::new();
        let mut ranks = Vec::new();
        for case in &dataset.cases {
            let ranked: Vec<String> = strategy(case).into_iter().map(|(url, _)| url).collect();
            let observation = observe(&ranked, &case.gold, limit);
            ranks.push(observation.rank);
            observations.push(observation);
        }
        let metrics = aggregate(&observations);
        let name = (*name).to_string();
        if options.rows {
            for (index, case) in dataset.cases.iter().enumerate() {
                per_query_rows.push(json!({
                    "dataset": dataset.name,
                    "strategy": name,
                    "id": case.id,
                    "gold": case.gold,
                    "rank": ranks[index],
                }));
            }
        }
        strategies.push(StrategyOutcome {
            name,
            observations,
            metrics,
            ranks,
        });
    }

    let reference = strategies
        .iter()
        .find(|outcome| outcome.name == "existing")
        .expect("existing strategy always present");
    let mut comparisons = Vec::new();
    for outcome in &strategies {
        if outcome.name == "existing" {
            continue;
        }
        let (mut wins, mut losses, mut ties) = (0usize, 0usize, 0usize);
        for (left, right) in outcome.observations.iter().zip(&reference.observations) {
            let epsilon = 1e-12;
            if left.mrr > right.mrr + epsilon {
                wins += 1;
            } else if left.mrr + epsilon < right.mrr {
                losses += 1;
            } else {
                ties += 1;
            }
        }
        comparisons.push(json!({
            "dataset": dataset.name,
            "strategy": outcome.name,
            "delta_mrr": outcome.metrics.mrr10 - reference.metrics.mrr10,
            "delta_ndcg": outcome.metrics.ndcg10 - reference.metrics.ndcg10,
            "wins": wins,
            "losses": losses,
            "ties": ties,
            "sign_test_p": sign_test(wins, losses),
        }));
    }

    let mut results: Vec<Value> = strategies
        .iter()
        .map(|outcome| {
            json!({
                "dataset": dataset.name,
                "strategy": outcome.name,
                "queries": outcome.observations.len(),
                "hit@1": outcome.metrics.hit1,
                "hit@5": outcome.metrics.hit5,
                "hit@10": outcome.metrics.hit10,
                "mrr@10": outcome.metrics.mrr10,
                "ndcg@10": outcome.metrics.ndcg10,
            })
        })
        .collect();
    results.sort_by(|a, b| {
        b["ndcg@10"]
            .as_f64()
            .partial_cmp(&a["ndcg@10"].as_f64())
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let ranks_by_strategy: Vec<(String, Vec<usize>)> = strategies
        .iter()
        .map(|outcome| (outcome.name.clone(), outcome.ranks.clone()))
        .collect();
    let _ = &ranks_by_strategy;

    json!({
        "dataset": dataset.name,
        "queries": dataset.cases.len(),
        "sources": dataset.sources,
        "fixtures": dataset.fixtures.iter().map(|(path, digest)| json!({"path": path, "sha256": digest})).collect::<Vec<_>>(),
        "results": results,
        "comparisons": comparisons,
        "rows": per_query_rows,
    })
}

fn main() {
    let options = match Options::parse() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("fusion_lab: {message}");
            std::process::exit(2);
        }
    };
    let root = options.repo.clone();
    let questions = match load_questions(&root) {
        Ok(questions) => questions,
        Err(message) => {
            eprintln!("fusion_lab: {message}");
            std::process::exit(2);
        }
    };

    let mut reports = Vec::new();
    for name in &options.datasets {
        let dataset = match name.as_str() {
            "providers" => provider_dataset(&root, "providers", &["exa", "tavily"], &questions),
            "providers4" => provider_dataset(
                &root,
                "providers4",
                &["exa", "tavily", "searxng", "rustxng"],
                &questions,
            ),
            "providers3" => {
                provider_dataset(&root, "providers3", &["searxng", "rustxng"], &questions)
            }
            "dev40" => dev40_dataset(&root, options.limit),
            "recorded100" => recorded100_dataset(&root, &questions),
            other => {
                eprintln!("fusion_lab: unknown dataset {other}");
                std::process::exit(2);
            }
        };
        match dataset {
            Ok(dataset) => reports.push(measure(&dataset, &options)),
            Err(message) => {
                eprintln!("fusion_lab: {message}");
                std::process::exit(2);
            }
        }
    }

    let report = json!({
        "harness": "fusion_lab",
        "version": 1,
        "scope": "Offline replay of fixtures already in the repository; no provider or network access.",
        "parameters": {
            "limit": options.limit,
            "rrf_k": options.rrf_k,
            "diversity_lambda": options.lambda,
            "source_weights": options.weights,
            "relevance_weight": options.relevance_weight,
            "authority_weight": options.authority_weight,
        },
        "datasets": reports,
    });

    let text = serde_json::to_string_pretty(&report).expect("report serialises");
    match &options.out {
        Some(path) => {
            if let Err(error) = std::fs::write(path, &text) {
                eprintln!("fusion_lab: {}: {error}", path.display());
                std::process::exit(1);
            }
            println!("wrote {}", path.display());
        }
        None => println!("{text}"),
    }

    if let Some(datasets) = report["datasets"].as_array() {
        for dataset in datasets {
            println!(
                "\n## {} (n = {})",
                dataset["dataset"].as_str().unwrap_or(""),
                dataset["queries"]
            );
            println!(
                "| strategy | Hit@1 | Hit@5 | Hit@10 | MRR@10 | nDCG@10 | dMRR vs existing | p |"
            );
            println!("| --- | --- | --- | --- | --- | --- | --- | --- |");
            let mut comparisons: HashMap<&str, &Value> = HashMap::new();
            if let Some(list) = dataset["comparisons"].as_array() {
                for entry in list {
                    if let Some(key) = entry["strategy"].as_str() {
                        comparisons.insert(key, entry);
                    }
                }
            }
            if let Some(rows) = dataset["results"].as_array() {
                for row in rows {
                    let strategy = row["strategy"].as_str().unwrap_or("");
                    let (delta, p) = comparisons
                        .get(strategy)
                        .map(|entry| {
                            (
                                format!("{:+.4}", entry["delta_mrr"].as_f64().unwrap_or(0.0)),
                                format!("{:.4}", entry["sign_test_p"].as_f64().unwrap_or(1.0)),
                            )
                        })
                        .unwrap_or_else(|| ("-".to_string(), "-".to_string()));
                    println!(
                        "| {} | {:.2} | {:.2} | {:.2} | {:.4} | {:.4} | {} | {} |",
                        strategy,
                        row["hit@1"].as_f64().unwrap_or(0.0),
                        row["hit@5"].as_f64().unwrap_or(0.0),
                        row["hit@10"].as_f64().unwrap_or(0.0),
                        row["mrr@10"].as_f64().unwrap_or(0.0),
                        row["ndcg@10"].as_f64().unwrap_or(0.0),
                        delta,
                        p
                    );
                }
            }
        }
    }
}
