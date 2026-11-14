//! Bounded access to the official catalogue. Unknown markup is never a negative result.
use super::standards::{Card, CardKind, Catalog, Key};
use crate::core::Visibility;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, future::Future, pin::Pin};

const BASE: &str = "https://protect.gost.ru";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Outcome {
    Found,
    NotFound,
    Restricted,
    InvalidDesignation,
    RequestLimit,
    PageLimit,
    NetworkError,
    HttpError(u16),
    DisallowedRedirect,
    InvalidMarkup,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Lookup {
    pub key: Key,
    pub catalog: Catalog,
    pub outcome: Outcome,
    pub requests: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Response {
    pub status: u16,
    pub location: Option<String>,
    pub body: String,
}
pub trait Transport {
    fn get<'a>(
        &'a self,
        url: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Response, ()>> + Send + 'a>>;
}
pub struct Http(reqwest::Client);
impl Transport for Http {
    fn get<'a>(
        &'a self,
        url: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Response, ()>> + Send + 'a>> {
        Box::pin(async move {
            let mut response = self.0.get(url).send().await.map_err(|_| ())?;
            let status = response.status().as_u16();
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|_| ())? {
                if bytes.len() + chunk.len() > 2_000_000 {
                    return Err(());
                }
                bytes.extend_from_slice(&chunk);
            }
            let body = String::from_utf8(bytes).map_err(|_| ())?;
            Ok(Response {
                status,
                location,
                body,
            })
        })
    }
}

pub struct CatalogHttp<T = Http> {
    transport: T,
    max_requests: usize,
    max_pages: usize,
    retrieved_at: String,
    requests: Vec<String>,
    cache: HashMap<String, Response>,
}
impl CatalogHttp<Http> {
    pub fn new(
        max_requests: usize,
        max_pages: usize,
        retrieved_at: impl Into<String>,
    ) -> Result<Self, reqwest::Error> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .timeout(std::time::Duration::from_secs(15))
            .build()?;
        Ok(Self::with_transport(
            Http(client),
            max_requests,
            max_pages,
            retrieved_at,
        ))
    }
}
impl<T: Transport> CatalogHttp<T> {
    pub fn with_transport(
        transport: T,
        max_requests: usize,
        max_pages: usize,
        retrieved_at: impl Into<String>,
    ) -> Self {
        Self {
            transport,
            max_requests,
            max_pages,
            retrieved_at: retrieved_at.into(),
            requests: Vec::new(),
            cache: HashMap::new(),
        }
    }
    pub fn total_requests(&self) -> usize {
        self.requests.len()
    }
    async fn fetch(&mut self, path: &str) -> Result<Response, Outcome> {
        let mut path = path.to_string();
        let mut seen = Vec::new();
        loop {
            if let Some(response) = self.cache.get(&path) {
                return Ok(response.clone());
            }
            if seen.contains(&path) {
                return Err(Outcome::DisallowedRedirect);
            }
            if self.requests.len() >= self.max_requests {
                return Err(Outcome::RequestLimit);
            }
            seen.push(path.clone());
            let url = format!("{BASE}{path}");
            self.requests.push(url.clone());
            let response = self
                .transport
                .get(&url)
                .await
                .map_err(|_| Outcome::NetworkError)?;
            if (300..400).contains(&response.status) {
                let location = response
                    .location
                    .as_deref()
                    .ok_or(Outcome::DisallowedRedirect)?;
                let next = location.strip_prefix(BASE).unwrap_or(location);
                // Only the originally requested resource is permitted, including its complete query.
                if !allowed_redirect(&path, next, self.max_pages) {
                    return Err(Outcome::DisallowedRedirect);
                }
                path = next.to_string();
                continue;
            }
            if response.status != 200 {
                return Err(Outcome::HttpError(response.status));
            }
            self.cache.insert(path, response.clone());
            return Ok(response);
        }
    }
    pub async fn lookup(&mut self, visibility: Visibility, key: &Key) -> Lookup {
        let start = self.requests.len();
        let mut cards = Vec::new();
        let outcome = if visibility != Visibility::Public {
            Outcome::Restricted
        } else if !valid_key(key) {
            Outcome::InvalidDesignation
        } else {
            self.search(key, &mut cards, false).await
        };
        Lookup {
            key: key.clone(),
            catalog: Catalog {
                retrieved_at: self.retrieved_at.clone(),
                cards,
            },
            outcome,
            requests: self.requests[start..].to_vec(),
        }
    }
    pub async fn lookup_family_variants(&mut self, visibility: Visibility, key: &Key) -> Lookup {
        let start = self.requests.len();
        let mut cards = Vec::new();
        let outcome = if visibility != Visibility::Public {
            Outcome::Restricted
        } else if !valid_key(key) || key.year.is_none() {
            Outcome::InvalidDesignation
        } else {
            self.search(key, &mut cards, true).await
        };
        Lookup {
            key: key.clone(),
            catalog: Catalog {
                retrieved_at: self.retrieved_at.clone(),
                cards,
            },
            outcome,
            requests: self.requests[start..].to_vec(),
        }
    }
    async fn search(&mut self, key: &Key, cards: &mut Vec<Card>, variants: bool) -> Outcome {
        let mut page = 1usize;
        let mut total = None;
        let mut seen = std::collections::HashSet::new();
        loop {
            if page > self.max_pages {
                return Outcome::PageLimit;
            }
            let path = format!("/gost?year=0&month=0&search={}&page={page}", key.number);
            let request_start = self.requests.len();
            let response = match self.fetch(&path).await {
                Ok(v) => v,
                Err(e) => return e,
            };
            if self.requests[request_start..].iter().any(|url| {
                reqwest::Url::parse(url).ok().and_then(|u| {
                    u.query_pairs()
                        .find(|(k, _)| k == "page")
                        .and_then(|(_, v)| v.parse::<usize>().ok())
                }) != Some(page)
            }) {
                return Outcome::InvalidMarkup;
            }
            let (count, entries) = match parse_list(&response.body) {
                Some(v) => v,
                None => return Outcome::InvalidMarkup,
            };
            if total.is_some_and(|n| n != count) {
                return Outcome::InvalidMarkup;
            }
            total = Some(count);
            let expected = count.saturating_sub((page - 1) * 20).min(20);
            if entries.len() != expected {
                return Outcome::InvalidMarkup;
            }
            for (id, designation) in entries {
                if !seen.insert(id.clone()) {
                    return Outcome::InvalidMarkup;
                }
                let candidate =
                    super::standards::parse_prefix(&designation).and_then(|(candidate, end)| {
                        (end == designation.len()
                            && valid_key(&candidate)
                            && candidate.number == key.number
                            && candidate.year == key.year
                            && candidate.family != key.family)
                            .then_some(candidate)
                    });
                if (!variants && matches_key(&designation, key))
                    || (variants && candidate.is_some())
                {
                    let path = format!("/gost/details/{id}");
                    let response = match self.fetch(&path).await {
                        Ok(v) => v,
                        Err(e) => return e,
                    };
                    match parse_card(
                        &response.body,
                        &path,
                        candidate.as_ref().filter(|_| variants).unwrap_or(key),
                    ) {
                        Some(card) => cards.push(card),
                        None => return Outcome::InvalidMarkup,
                    }
                }
            }
            if page * 20 >= count {
                return if cards.is_empty() {
                    Outcome::NotFound
                } else {
                    Outcome::Found
                };
            }
            page += 1;
        }
    }
}
fn allowed_redirect(current: &str, next: &str, max_pages: usize) -> bool {
    let parse = |path: &str| reqwest::Url::parse(&format!("{BASE}{path}")).ok();
    let (Some(a), Some(b)) = (parse(current), parse(next)) else {
        return false;
    };
    if b.origin() != a.origin()
        || b.fragment().is_some()
        || b.username() != ""
        || b.password().is_some()
    {
        return false;
    }
    if a.path().starts_with("/gost/details/") {
        return a == b;
    }
    if !matches!(a.path(), "/gost" | "/gost/") || !matches!(b.path(), "/gost" | "/gost/") {
        return false;
    }
    let params = |url: &reqwest::Url| -> Option<(String, usize)> {
        let pairs: Vec<_> = url
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        if pairs.len() != 4 {
            return None;
        }
        let get = |name: &str| {
            let values: Vec<_> = pairs.iter().filter(|(k, _)| k == name).collect();
            (values.len() == 1).then(|| values[0].1.clone())
        };
        if get("year")? != "0" || get("month")? != "0" {
            return None;
        }
        let page: usize = get("page")?.parse().ok()?;
        if page == 0 || page > max_pages {
            return None;
        }
        Some((get("search")?, page))
    };
    matches!((params(&a), params(&b)), (Some((x, _)), Some((y, _))) if x == y)
}
fn valid_key(key: &Key) -> bool {
    let families = [
        "ГОСТ",
        "ГОСТ Р",
        "ГОСТ ИСО",
        "ГОСТ Р ИСО",
        "ГОСТ МЭК",
        "ГОСТ Р МЭК",
        "ГОСТ ИСО МЭК",
        "ГОСТ Р ИСО МЭК",
        "ГОСТ ISO",
        "ГОСТ IEC",
        "ГОСТ Р ISO",
        "ГОСТ Р IEC",
    ];
    families.contains(&key.family.as_str())
        && !key.number.is_empty()
        && key.number.len() <= 40
        && key
            .number
            .bytes()
            .all(|b| b.is_ascii_digit() || b == b'.' || b == b'-')
        && key
            .number
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_digit())
        && key
            .year
            .as_ref()
            .is_none_or(|y| matches!(y.len(), 2 | 4) && y.bytes().all(|b| b.is_ascii_digit()))
}
fn matches_key(designation: &str, key: &Key) -> bool {
    let designation = designation.replace('/', " ").replace(['–', '—'], "-");
    let designation = designation
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_uppercase();
    let prefix = format!("{} {}", key.family, key.number);
    match &key.year {
        Some(year) => designation == format!("{prefix}-{year}"),
        None => {
            designation == prefix
                || designation
                    .strip_prefix(&format!("{prefix}-"))
                    .is_some_and(|y| {
                        matches!(y.len(), 2 | 4) && y.bytes().all(|b| b.is_ascii_digit())
                    })
        }
    }
}
fn parse_list(html: &str) -> Option<(usize, Vec<(String, String)>)> {
    let after = html.split_once("Найдено:")?.1;
    let count: usize = after
        .split_once('>')?
        .1
        .split_once('<')?
        .0
        .trim()
        .parse()
        .ok()?;
    if count == 0 {
        return html
            .contains("Документы не найдены")
            .then_some((0, Vec::new()));
    }
    let desktop = html.split_once("<table")?.1.split_once("</table>")?.0;
    let mut result = Vec::new();
    for part in desktop.split("href=\"/gost/details/").skip(1) {
        let (id, rest) = part.split_once('"')?;
        if id.len() != 36 || !id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
            return None;
        }
        let anchor = rest.split_once("</a>")?.0;
        let designation = anchor
            .rsplit_once("<span>")?
            .1
            .split_once("</span>")?
            .0
            .trim();
        result.push((id.to_string(), designation.to_string()));
    }
    Some((count, result))
}
fn field(html: &str, label: &str) -> Option<String> {
    let marker = format!(">{label}</span>");
    let rest = html.split_once(&marker)?.1;
    let container = rest.split_once("</div>")?.0;
    let p = container
        .split_once("<p ")?
        .1
        .split_once('>')?
        .1
        .split_once("</p>")?
        .0;
    if p.contains('<') {
        return None;
    }
    Some(
        p.trim()
            .replace("&amp;", "&")
            .replace("&quot;", "\"")
            .replace("&lt;", "<")
            .replace("&gt;", ">"),
    )
}
fn optional_field(html: &str, label: &str) -> Option<String> {
    field(html, label).filter(|s| !s.is_empty() && s != "-")
}
fn parse_card(html: &str, path: &str, key: &Key) -> Option<Card> {
    let designation = field(html, "Обозначение")?;
    if !matches_key(&designation, key) {
        return None;
    }
    let effective_from = optional_field(html, "Дата введения в действие").and_then(|s| {
        let p: Vec<_> = s.split('.').collect();
        if p.len() == 3
            && p[0].len() == 2
            && p[1].len() == 2
            && p[2].len() == 4
            && p.iter().all(|x| x.bytes().all(|b| b.is_ascii_digit()))
        {
            let iso = format!("{}-{}-{}", p[2], p[1], p[0]);
            chrono::NaiveDate::parse_from_str(&iso, "%Y-%m-%d")
                .ok()
                .map(|_| iso)
        } else {
            None
        }
    });
    let badge = html
        .split_once("<!-- Бейдж статуса -->")?
        .1
        .split_once("</span>\n</div>")?
        .0
        .rsplit_once("</span>")?
        .1
        .trim()
        .to_string();
    if badge.is_empty() || badge.contains('<') {
        return None;
    }
    Some(Card {
        kind: CardKind::Standard,
        full_designation: field(html, "Полное обозначение")?,
        title: field(html, "Заглавие на русском языке")?,
        scope: field(html, "Область применения").unwrap_or_default(),
        designation,
        url: format!("{BASE}{path}"),
        temperature_scope: None,
        effective_from,
        replaces: optional_field(html, "Обозначение заменяемого(ых)"),
        replacement: optional_field(html, "Обозначение заменяющего"),
        status_at_retrieval: badge,
        rf_note: None,
        clauses: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    struct Fake(Mutex<Vec<Response>>);
    impl Transport for Fake {
        fn get<'a>(
            &'a self,
            _: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<Response, ()>> + Send + 'a>> {
            Box::pin(async move {
                let mut r = self.0.lock().unwrap();
                if r.is_empty() {
                    Err(())
                } else {
                    Ok(r.remove(0))
                }
            })
        }
    }
    fn response(body: &str) -> Response {
        Response {
            status: 200,
            location: None,
            body: body.into(),
        }
    }
    fn redirect(location: &str) -> Response {
        Response {
            status: 302,
            location: Some(location.into()),
            body: String::new(),
        }
    }
    fn key() -> Key {
        Key {
            family: "ГОСТ".into(),
            number: "9013".into(),
            year: Some("59".into()),
        }
    }
    fn list(count: usize, start: usize, n: usize, matched: bool) -> String {
        let mut s = format!("Найдено: <strong>{count}</strong><table>");
        for i in start..start + n {
            let designation = if matched && i == 0 {
                "ГОСТ 9013-59"
            } else {
                "ГОСТ 1234-20"
            };
            s.push_str(&format!("<a href=\"/gost/details/{i:08x}-0000-0000-0000-000000000000\"><span>{designation}</span></a>"));
        }
        s.push_str("</table>");
        s
    }
    fn card() -> String {
        let mut s = "<!-- Бейдж статуса --><span><span></span>Действует</span>\n</div>".to_string();
        for (label, value) in [
            ("Обозначение", "ГОСТ 9013-59"),
            ("Полное обозначение", "ГОСТ 9013-59"),
            ("Заглавие на русском языке", "Стандарт"),
            ("Область применения", "Область"),
        ] {
            s.push_str(&format!(
                "<div><span>{label}</span><p class=\"value\">{value}</p></div>"
            ));
        }
        s
    }
    #[test]
    fn card_preserves_available_details_without_inventing_missing_fields() {
        let html = format!(
            "{}<div><span>Дата введения в действие</span><p class=\"value\">01.02.2020</p></div><div><span>Обозначение заменяющего</span><p class=\"value\">ГОСТ 9013-2020</p></div>",
            card()
        );
        let parsed = parse_card(&html, "/gost/details/id", &key()).unwrap();
        assert_eq!(parsed.effective_from.as_deref(), Some("2020-02-01"));
        assert_eq!(parsed.replacement.as_deref(), Some("ГОСТ 9013-2020"));
        assert!(parsed.replaces.is_none());
        assert!(parsed.rf_note.is_none());
        let invalid = html.replace("01.02.2020", "31.02.2020");
        assert!(
            parse_card(&invalid, "/gost/details/id", &key())
                .unwrap()
                .effective_from
                .is_none()
        );
    }
    #[tokio::test]
    async fn bounded_requests_pagination_and_cache() {
        let fake = Fake(Mutex::new(vec![
            response(&list(21, 0, 20, true)),
            response(&card()),
            response(&list(21, 20, 1, false)),
        ]));
        let mut client = CatalogHttp::with_transport(fake, 3, 3, "2026-11-13");
        assert!(matches!(
            client.lookup(Visibility::Private, &key()).await.outcome,
            Outcome::Restricted
        ));
        assert!(matches!(
            client.lookup(Visibility::Unknown, &key()).await.outcome,
            Outcome::Restricted
        ));
        let found = client.lookup(Visibility::Public, &key()).await;
        assert!(matches!(found.outcome, Outcome::Found));
        assert_eq!(found.catalog.cards.len(), 1);
        assert_eq!(client.total_requests(), 3);
        assert!(found.requests.iter().all(|s| !s.contains("Стандарт")));
        assert!(matches!(
            client.lookup(Visibility::Public, &key()).await.outcome,
            Outcome::Found
        ));
        assert_eq!(client.total_requests(), 3);
    }
    #[tokio::test]
    async fn exhausted_catalogue_does_not_consume_model_attempts() {
        use crate::{
            consistency::{ReviewOptions, review_with_catalog},
            core::{Document, ErrorCategory},
            runtime::{Runtime, grok::Model},
        };
        struct Replay;
        impl Model for Replay {
            async fn complete(&self, _: &str) -> Result<String, ErrorCategory> {
                Ok(r#"{"claims":[],"priority_rules":[],"refinements":[],"standard_mentions":[],"role":"bibliography","outcome":"insufficient_evidence"}"#.into())
            }
        }
        let document = Document {
            markdown: "Перечень: ГОСТ 9013-59.".into(),
            visibility: Visibility::Public,
        };
        let catalog = Catalog {
            retrieved_at: "2026-11-13".into(),
            cards: vec![],
        };
        let runtime = Runtime::new(Replay, std::time::Duration::from_secs(1));
        for limit in [0, 2] {
            let mut client = CatalogHttp::with_transport(
                Fake(Mutex::new(vec![
                    response(&list(1, 0, 1, true)),
                    response(&card()),
                ])),
                limit,
                1,
                &catalog.retrieved_at,
            );
            let report = review_with_catalog(
                &document,
                ReviewOptions {
                    model_attempts: 3,
                    ..Default::default()
                },
                &runtime,
                &catalog,
                Some(&mut client),
            )
            .await
            .unwrap();
            assert_eq!(report.catalog_requests, limit);
            assert_eq!(report.model_attempts, 3);
            assert!(matches!(
                (limit, &report.catalog_trace[0].outcome),
                (0, Outcome::RequestLimit) | (2, Outcome::Found)
            ));
        }
    }

    #[tokio::test]
    async fn incomplete_and_exhaustive_search_are_distinct() {
        let empty = response("Найдено: <strong>0</strong><h3>Документы не найдены</h3>");
        let mut client =
            CatalogHttp::with_transport(Fake(Mutex::new(vec![empty])), 2, 2, "2026-11-13");
        assert!(matches!(
            client.lookup(Visibility::Public, &key()).await.outcome,
            Outcome::NotFound
        ));
        let mut client = CatalogHttp::with_transport(
            Fake(Mutex::new(vec![response(&list(21, 0, 20, false))])),
            3,
            1,
            "2026-11-13",
        );
        assert!(matches!(
            client.lookup(Visibility::Public, &key()).await.outcome,
            Outcome::PageLimit
        ));
        let mut client = CatalogHttp::with_transport(
            Fake(Mutex::new(vec![response("changed markup")])),
            3,
            3,
            "2026-11-13",
        );
        assert!(matches!(
            client.lookup(Visibility::Public, &key()).await.outcome,
            Outcome::InvalidMarkup
        ));
        let mut invalid = key();
        invalid.number = "9013&private=text".into();
        assert!(matches!(
            client.lookup(Visibility::Public, &invalid).await.outcome,
            Outcome::InvalidDesignation
        ));
        assert_eq!(client.total_requests(), 1);
    }
    #[tokio::test]
    async fn redirects_consume_budget_and_reject_external() {
        let path = "/gost?year=0&month=0&search=9013&page=2";
        for (location, budget, expected) in
            [("https://evil.example/", 3, 1), (path, 1, 1), (path, 2, 2)]
        {
            let fake = Fake(Mutex::new(vec![
                redirect(location),
                response(&list(21, 20, 1, false)),
            ]));
            let mut client = CatalogHttp::with_transport(fake, budget, 3, "2026-11-13");
            let result = client.lookup(Visibility::Public, &key()).await;
            assert_eq!(client.total_requests(), expected);
            assert!(!matches!(
                result.outcome,
                Outcome::Found | Outcome::NotFound
            ));
        }
    }

    #[tokio::test]
    async fn exact_miss_preserves_status_and_shows_title_confirmed_family_variant() {
        use crate::{
            consistency::{ReviewOptions, review_with_catalog},
            core::{Document, ErrorCategory},
            runtime::{Runtime, grok::Model},
        };
        struct Replay;
        impl Model for Replay {
            async fn complete(&self, _: &str) -> Result<String, ErrorCategory> {
                Ok(r#"{"claims":[],"priority_rules":[],"refinements":[],"standard_mentions":[],"role":"bibliography","outcome":"insufficient_evidence"}"#.into())
            }
        }
        let list = list(1, 0, 1, true).replace("ГОСТ 9013-59", "ГОСТ 34441-2018");
        let card = card()
            .replace("ГОСТ 9013-59", "ГОСТ 34441-2018")
            .replace(
                "Стандарт",
                "Лифты. Диспетчерский контроль. Общие технические требования",
            )
            .replace("Действует", "Заменен")
            + "<div><span>Обозначение заменяющего</span><p class=\"value\">ГОСТ 34441-2024</p></div>";
        let mut client = CatalogHttp::with_transport(
            Fake(Mutex::new(vec![response(&list), response(&card)])),
            2,
            2,
            "2026-11-13",
        );
        let document = Document {
            markdown:
                "ГОСТ Р 34441-2018 «Лифты. Диспетчерский контроль. Общие технические требования»"
                    .into(),
            visibility: Visibility::Public,
        };
        let catalog = Catalog {
            retrieved_at: "2026-11-13".into(),
            cards: vec![],
        };
        let runtime = Runtime::new(Replay, std::time::Duration::from_secs(1));
        let report = review_with_catalog(
            &document,
            ReviewOptions {
                model_attempts: 3,
                ..Default::default()
            },
            &runtime,
            &catalog,
            Some(&mut client),
        )
        .await
        .unwrap();
        let row = &report.standard_references[0];
        assert!(matches!(
            row.existence,
            super::super::standards::Existence::NotFoundInOfficialSearch
        ));
        assert!(row.cards.is_empty());
        assert_eq!(row.confirmed_family_variants.len(), 1);
        assert_eq!(
            row.confirmed_family_variants[0].status_at_retrieval,
            "Заменен"
        );
        assert_eq!(
            row.confirmed_family_variants[0].replacement.as_deref(),
            Some("ГОСТ 34441-2024")
        );
        assert_eq!(client.total_requests(), 2);
        assert!(matches!(report.catalog_trace[0].outcome, Outcome::NotFound));
        for (title, limit) in [
            ("Совершенно другое название", 2),
            (
                "Лифты. Диспетчерский контроль. Общие технические требования для пассажиров",
                2,
            ),
            (
                "Лифты. Диспетчерский контроль. Общие технические требования",
                1,
            ),
        ] {
            let mut client = CatalogHttp::with_transport(
                Fake(Mutex::new(vec![response(&list), response(&card)])),
                limit,
                2,
                "2026-11-13",
            );
            let document = Document {
                markdown: format!("ГОСТ Р 34441-2018 «{title}»"),
                visibility: Visibility::Public,
            };
            let report = review_with_catalog(
                &document,
                ReviewOptions {
                    model_attempts: 3,
                    ..Default::default()
                },
                &runtime,
                &catalog,
                Some(&mut client),
            )
            .await
            .unwrap();
            assert!(matches!(
                report.standard_references[0].existence,
                super::super::standards::Existence::NotFoundInOfficialSearch
            ));
            assert!(
                report.standard_references[0]
                    .confirmed_family_variants
                    .is_empty()
            );
        }
        let mut client = CatalogHttp::with_transport(
            Fake(Mutex::new(vec![response(&list), response("invalid card")])),
            2,
            2,
            "2026-11-13",
        );
        let report = review_with_catalog(
            &document,
            ReviewOptions {
                model_attempts: 3,
                ..Default::default()
            },
            &runtime,
            &catalog,
            Some(&mut client),
        )
        .await
        .unwrap();
        assert!(matches!(
            report.standard_references[0].existence,
            super::super::standards::Existence::NotFoundInOfficialSearch
        ));
        assert!(
            report.standard_references[0]
                .confirmed_family_variants
                .is_empty()
        );
        assert!(matches!(
            report.catalog_trace[1].outcome,
            Outcome::InvalidMarkup
        ));
        let mut client = CatalogHttp::with_transport(
            Fake(Mutex::new(vec![
                response(&list),
                response("Найдено: <strong>0</strong><h3>Документы не найдены</h3>"),
                response(&card),
            ])),
            20,
            2,
            "2026-11-13",
        );
        let document = Document { markdown: "ГОСТ Р 34441-2018 «Лифты. Диспетчерский контроль. Общие технические требования»\n\nГОСТ 9999-2018".into(), visibility: Visibility::Public };
        let report = review_with_catalog(
            &document,
            ReviewOptions {
                model_attempts: 3,
                ..Default::default()
            },
            &runtime,
            &catalog,
            Some(&mut client),
        )
        .await
        .unwrap();
        assert_eq!(client.total_requests(), 3);
        assert!(report.catalog_trace[1].requests[0].contains("search=9999"));
        assert_eq!(
            report.standard_references[0]
                .confirmed_family_variants
                .len(),
            1
        );
        let mut responses = vec![response(&list)];
        responses.extend(
            (0..19).map(|_| response("Найдено: <strong>0</strong><h3>Документы не найдены</h3>")),
        );
        let mut client =
            CatalogHttp::with_transport(Fake(Mutex::new(responses)), 20, 2, "2026-11-13");
        let mut markdown =
            "ГОСТ Р 34441-2018 «Лифты. Диспетчерский контроль. Общие технические требования»"
                .to_owned();
        for n in 50000..50019 {
            markdown.push_str(&format!("\n\nГОСТ {n}-2018"));
        }
        let document = Document {
            markdown,
            visibility: Visibility::Public,
        };
        let report = review_with_catalog(
            &document,
            ReviewOptions {
                model_attempts: 3,
                ..Default::default()
            },
            &runtime,
            &catalog,
            Some(&mut client),
        )
        .await
        .unwrap();
        assert_eq!(client.total_requests(), 20);
        assert!(matches!(
            report.standard_references[0].existence,
            super::super::standards::Existence::NotFoundInOfficialSearch
        ));
        assert!(
            report.standard_references[0]
                .confirmed_family_variants
                .is_empty()
        );
        assert!(matches!(
            report.catalog_trace[20].outcome,
            Outcome::RequestLimit
        ));
    }
}
