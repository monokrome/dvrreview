use anyhow::{Context, Result, bail};
use regex_lite::Regex;
use serde::Deserialize;
use std::collections::HashMap;
use tokio::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct TmdbResult {
    pub id: i32,
    pub media_type: String,
    pub title: String,
    pub year: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ParsedTitle {
    pub clean_title: String,
    pub year: Option<i32>,
}

pub fn parse_title(claimed_title: &str) -> ParsedTitle {
    let re = Regex::new(r"^(.+?)\s*\((\d{4})\)\s*$").unwrap();
    if let Some(caps) = re.captures(claimed_title) {
        let clean = caps.get(1).unwrap().as_str().trim().to_string();
        let year: i32 = caps.get(2).unwrap().as_str().parse().unwrap();
        ParsedTitle {
            clean_title: clean,
            year: Some(year),
        }
    } else {
        ParsedTitle {
            clean_title: claimed_title.trim().to_string(),
            year: None,
        }
    }
}

#[derive(Deserialize)]
struct SearchResponse {
    results: Vec<SearchResult>,
}

#[derive(Deserialize)]
struct SearchResult {
    id: i32,
    media_type: Option<String>,
    // TV shows use "name", movies use "title"
    name: Option<String>,
    title: Option<String>,
    first_air_date: Option<String>,
    release_date: Option<String>,
}

impl SearchResult {
    fn display_title(&self) -> String {
        self.name
            .clone()
            .or_else(|| self.title.clone())
            .unwrap_or_default()
    }

    fn year(&self) -> Option<i32> {
        let date = self
            .first_air_date
            .as_deref()
            .or(self.release_date.as_deref())?;
        date.split('-').next()?.parse().ok()
    }

    fn resolved_media_type(&self) -> Option<&str> {
        self.media_type.as_deref()
    }
}

struct RateLimiter {
    timestamps: Vec<Instant>,
    max_requests: usize,
    window: Duration,
}

impl RateLimiter {
    fn new(max_requests: usize, window: Duration) -> Self {
        Self {
            timestamps: Vec::new(),
            max_requests,
            window,
        }
    }

    async fn acquire(&mut self) {
        let now = Instant::now();
        self.timestamps.retain(|t| now.duration_since(*t) < self.window);

        if self.timestamps.len() >= self.max_requests {
            let oldest = self.timestamps[0];
            let wait = self.window - now.duration_since(oldest);
            tokio::time::sleep(wait).await;
            self.timestamps.retain(|t| Instant::now().duration_since(*t) < self.window);
        }

        self.timestamps.push(Instant::now());
    }
}

type CacheKey = (String, Option<i32>, bool);

pub struct TmdbClient {
    http: reqwest::Client,
    api_key: String,
    cache: HashMap<CacheKey, Option<TmdbResult>>,
    limiter: RateLimiter,
}

impl TmdbClient {
    pub fn new(api_key: String) -> Result<Self> {
        if api_key.is_empty() {
            bail!("TMDB API key is empty");
        }

        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .context("Failed to build HTTP client")?;

        Ok(Self {
            http,
            api_key,
            cache: HashMap::new(),
            limiter: RateLimiter::new(35, Duration::from_secs(10)),
        })
    }

    pub async fn search(
        &mut self,
        clean_title: &str,
        year: Option<i32>,
        has_season: bool,
    ) -> Result<Option<TmdbResult>> {
        let key: CacheKey = (clean_title.to_string(), year, has_season);
        if let Some(cached) = self.cache.get(&key) {
            return Ok(cached.clone());
        }

        self.limiter.acquire().await;

        let mut request = self
            .http
            .get("https://api.themoviedb.org/3/search/multi")
            .query(&[("query", clean_title)])
            .bearer_auth(&self.api_key);

        if let Some(y) = year {
            request = request.query(&[("year", y.to_string())]);
        }

        let response = request
            .send()
            .await
            .context("TMDB API request failed")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            bail!("TMDB API returned {}: {}", status, body);
        }

        let search: SearchResponse = response
            .json()
            .await
            .context("Failed to parse TMDB response")?;

        let result = pick_best_result(&search.results, has_season);

        self.cache.insert(key, result.clone());
        Ok(result)
    }
}

fn pick_best_result(results: &[SearchResult], prefer_tv: bool) -> Option<TmdbResult> {
    if results.is_empty() {
        return None;
    }

    // Filter to tv/movie only (skip "person" results)
    let relevant: Vec<&SearchResult> = results
        .iter()
        .filter(|r| matches!(r.resolved_media_type(), Some("tv" | "movie")))
        .collect();

    if relevant.is_empty() {
        return None;
    }

    // If the file has season/episode info, prefer TV results
    let chosen = if prefer_tv {
        relevant
            .iter()
            .find(|r| r.resolved_media_type() == Some("tv"))
            .or(relevant.first())
    } else {
        relevant.first()
    };

    chosen.map(|r| TmdbResult {
        id: r.id,
        media_type: r.resolved_media_type().unwrap_or("movie").to_string(),
        title: r.display_title(),
        year: r.year(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_title_with_year() {
        let result = parse_title("Friends (1994)");
        assert_eq!(result.clean_title, "Friends");
        assert_eq!(result.year, Some(1994));
    }

    #[test]
    fn parse_title_without_year() {
        let result = parse_title("SpongeBob SquarePants");
        assert_eq!(result.clean_title, "SpongeBob SquarePants");
        assert_eq!(result.year, None);
    }

    #[test]
    fn parse_title_with_non_year_parens() {
        let result = parse_title("Some Show (UK)");
        assert_eq!(result.clean_title, "Some Show (UK)");
        assert_eq!(result.year, None);
    }

    #[test]
    fn parse_title_trims_whitespace() {
        let result = parse_title("  The Office  (2005) ");
        assert_eq!(result.clean_title, "The Office");
        assert_eq!(result.year, Some(2005));
    }

    #[test]
    fn pick_prefers_tv_when_has_season() {
        let results = vec![
            SearchResult {
                id: 100,
                media_type: Some("movie".to_string()),
                name: None,
                title: Some("Friends Movie".to_string()),
                first_air_date: None,
                release_date: Some("2020-01-01".to_string()),
            },
            SearchResult {
                id: 1668,
                media_type: Some("tv".to_string()),
                name: Some("Friends".to_string()),
                title: None,
                first_air_date: Some("1994-09-22".to_string()),
                release_date: None,
            },
        ];

        let result = pick_best_result(&results, true).unwrap();
        assert_eq!(result.id, 1668);
        assert_eq!(result.media_type, "tv");
        assert_eq!(result.title, "Friends");
        assert_eq!(result.year, Some(1994));
    }

    #[test]
    fn pick_takes_first_when_no_preference() {
        let results = vec![
            SearchResult {
                id: 100,
                media_type: Some("movie".to_string()),
                name: None,
                title: Some("Inception".to_string()),
                first_air_date: None,
                release_date: Some("2010-07-16".to_string()),
            },
        ];

        let result = pick_best_result(&results, false).unwrap();
        assert_eq!(result.id, 100);
        assert_eq!(result.media_type, "movie");
    }

    #[test]
    fn pick_skips_person_results() {
        let results = vec![
            SearchResult {
                id: 999,
                media_type: Some("person".to_string()),
                name: Some("Someone".to_string()),
                title: None,
                first_air_date: None,
                release_date: None,
            },
        ];

        assert!(pick_best_result(&results, false).is_none());
    }

    #[test]
    fn pick_empty_results() {
        assert!(pick_best_result(&[], false).is_none());
    }
}
