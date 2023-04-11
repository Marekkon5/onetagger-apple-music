#[macro_use] extern crate log;

use std::error::Error;
use std::sync::{Mutex, Arc};
use std::time::Duration;
use chrono::NaiveDate;
use fancy_regex::Regex;
use reqwest::blocking::{Client, ClientBuilder};
use reqwest::header::{HeaderMap, HeaderValue};
use serde::{Serialize, Deserialize};
use serde::de::DeserializeOwned;
use serde_json::Value;
use xmlparser::{Tokenizer, Token, ElementEnd};
use onetagger_tagger::{LyricsLine, LyricsLinePart, Lyrics, Track, TrackNumber, AutotaggerSourceBuilder, PlatformInfo, TaggerConfig, AutotaggerSource, AudioFileInfo, MatchingUtils, PlatformCustomOptions, PlatformCustomOptionValue, supported_tags};

const URL: &'static str = "https://amp-api.music.apple.com/v1/catalog";

#[derive(Clone)]
pub struct AppleMusic {
    client: Client,
    access_token: Arc<Mutex<Option<String>>>,
    catalog: Arc<Mutex<Option<String>>>,
    language: String
}

impl AppleMusic {
    /// Create new instance
    pub fn new(media_user_token: &str) -> AppleMusic {
        let mut headers = HeaderMap::new();
        headers.insert("Media-User-Token", HeaderValue::from_str(media_user_token).unwrap());
        headers.insert("Content-Type", HeaderValue::from_static("application/json"));
        headers.insert("Origin", HeaderValue::from_static("https://music.apple.com"));
        headers.insert("Referer", HeaderValue::from_static("https://music.apple.com/"));

        AppleMusic {
            access_token: Arc::new(Mutex::new(None)),
            catalog: Arc::new(Mutex::new(None)),
            client: ClientBuilder::new()
                .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/86.0.4240.183 Safari/537.36")
                .default_headers(headers)
                .build()
                .unwrap(),
            language: "en_GB".to_string(),
        }
    }

    /// Fetch the auth token
    pub fn fetch_token(&self) -> Result<(), Box<dyn Error>> {
        // Fetch the token
        debug!("Fetching Apple Music token");
        let body = self.client.get("https://music.apple.com/us/search").send()?.text()?;
        let re = Regex::new("(?<=index\\.)(.*?)(?=\\.js\")").unwrap();
        let index_js = re.captures(&body)?.ok_or("Unable to find index_js url")?.get(1).ok_or("Unable to get index_js url")?.as_str();
        let index_js = self.client.get(format!("https://music.apple.com/assets/index.{index_js}.js")).send()?.text()?;
        let re = Regex::new("(?=eyJh)(.*?)(?=\")").unwrap();
        let token = re.captures(&index_js)?.ok_or("Unable to find token")?.get(1).ok_or("Unable to find token")?.as_str();
        *self.access_token.lock().unwrap() = Some(token.to_string());
        // Fetch catalog
        let r: Value = self.client.get("https://amp-api.music.apple.com/v1/me/account?meta=subscription&challenge%5BsubscriptionCapabilities%5D=voice%2Cpremium")
            .bearer_auth(token)
            .send()?.json()?;
        // Check sub
        if !r["meta"]["subscription"]["active"].as_bool().unwrap_or(false) {
            return Err("Not subscribed!".into());
        }
        // Get storefront
        let storefront = r["meta"]["subscription"]["storefront"].as_str().ok_or("Unable to get storefront!")?;
        debug!("Storefront: {storefront}");
        *self.catalog.lock().unwrap() = Some(storefront.to_string());
        Ok(())
    }

    /// Do a GET request
    fn get<O: DeserializeOwned>(&self, path: &str, query: &[(&str, &str)]) -> Result<O, Box<dyn Error>> {
        // Get token
        if self.access_token.lock().unwrap().is_none() {
            self.fetch_token()?;
        }
        let token = self.access_token.lock().unwrap().as_ref().unwrap().to_string();
        let catalog = self.catalog.lock().unwrap().as_ref().unwrap().to_string();
        // Push
        let mut query = query.to_vec();
        query.push(("l", &self.language));
        let url = format!("{URL}/{catalog}/{path}");
        debug!("{url}");
        let r = self.client.get(url)
            .query(&query)
            .bearer_auth(&token)
            .send()?
            .json()?;
        Ok(r)
    }

    /// Search for tracks
    pub fn search(&self, query: &str) -> Result<SearchResults, Box<dyn Error>> {
        let r: SearchResultsResponse = self.get("search", &[
            ("groups", "song"),
            ("art[url]", "c,f"),
            ("extend", "artistUrl"),
            ("include[songs]", "artists,albums"),
            ("offset", "0"),
            ("term", query),
            ("types", "songs"),
            ("platform", "web"),
            ("limit", "50"),
            ("with", "serverBubbles,lyrics,lyricHighlights"),
            ("omit[resource]", "autos"),
        ])?;
        Ok(r.results)
    }

    /// Get the lyrics
    pub fn lyrics(&self, song_id: &str) -> Result<Lyrics, Box<dyn Error>> {
        let lyrics: Value = self.get(&format!("songs/{song_id}/lyrics"), &[])?;
        let ttml = lyrics["data"][0]["attributes"]["ttml"].as_str().ok_or("Missing TTML")?;
        Ok(Self::parse_ttml(ttml, &self.language)?)
    }

    /// Parse TTML from Apple Music
    fn parse_ttml(ttml: &str, language: &str) -> Result<Lyrics, Box<dyn Error>> {
        let mut is_body = false;
        let mut is_line_header = false;
        let mut is_synced_line = false;

        let mut paragraphs = vec![];
        let mut paragraph = vec![];
        let mut line = None;
        let mut part = None;

        for token in Tokenizer::from(ttml) {
            let token = token?;
            match token {
                Token::ElementStart { local, .. } => {
                    // Check for body start
                    if local.as_str() == "body" {
                        is_body = true;
                        continue;
                    }
                    if !is_body {
                        continue;
                    }

                    // Line start
                    if local.as_str() == "p" {
                        line = Some(LyricsLine { text: String::new(), start: None, end: None, parts: vec![] });
                        is_line_header = true;
                        is_synced_line = false;
                    }
                    // Synced line
                    if local.as_str() == "span" {
                        part = Some(LyricsLinePart { text: String::new(), start: None, end: None });
                        is_line_header = false;
                        is_synced_line = true;
                    }
                    
                },
                Token::Attribute { local, value, .. } => {
                    // Parse line attributes
                    if is_line_header {
                        let line = line.as_mut().unwrap();
                        match local.as_str() {
                            "begin" => line.start = Some(Lyrics::parse_lrc_timestamp(&value)?),
                            "end" => line.end = Some(Lyrics::parse_lrc_timestamp(&value)?),
                            _ => {}
                        }
                    }

                    // Parse synced line attribute
                    if is_synced_line {
                        let part = part.as_mut().unwrap();
                        match local.as_str() {
                            "begin" => part.start = Some(Lyrics::parse_lrc_timestamp(&value)?),
                            "end" => part.end = Some(Lyrics::parse_lrc_timestamp(&value)?),
                            _ => {}
                        }
                    }
                },
                Token::ElementEnd { end, .. } => {
                    match end {
                        // End of body
                        ElementEnd::Close(_, local) if local.as_str() == "body" =>  {
                            break;
                        },
                        // End of line
                        ElementEnd::Close(_, local) if local.as_str() == "p" => {
                            let mut line = line.take().unwrap();
                            // Merge text from parts
                            if line.text.is_empty() {
                                line.text = line.parts.iter().map(|p| p.text.as_str()).collect::<Vec<_>>().join(" ");
                            }
                            // Add line
                            is_line_header = false;
                            is_synced_line = false;
                            paragraph.push(line);
                        },
                        // End of part
                        ElementEnd::Close(_, local) if local.as_str() == "span" => {
                            is_synced_line = false;
                            line.as_mut().unwrap().parts.push(part.take().unwrap());
                        },
                        // End of paragraph
                        ElementEnd::Close(_, local) if local.as_str() == "div" => {
                            is_line_header = false;
                            is_synced_line = false;
                            paragraphs.push(paragraph.to_owned());
                            paragraph.clear();
                        }
                        _ => continue
                    }


                },
                Token::Text { text } => {
                    // Unsynced
                    if is_line_header {
                        line.as_mut().unwrap().text = text.as_str().to_string();
                    }
                    // Synced 
                    if is_synced_line {
                        part.as_mut().unwrap().text = text.as_str().to_string();
                    }
                    
                },
                _ => continue
            }
        }

        // Create lyrics
        Ok(Lyrics { paragraphs, language: language.to_owned() })
    }

}

impl AutotaggerSource for AppleMusic {
    fn match_track(&mut self, info: &AudioFileInfo, config: &TaggerConfig) -> Result<Option<(f64, Track)>, Box<dyn Error>> {
        let query = format!("{} {}", info.artist()?, info.title()?);
        let results = self.search(&query)?;
        let tracks: Vec<Track> = results.song.data.into_iter().map(|s| s.into()).collect();
        if let Some((acc, mut track)) = MatchingUtils::match_track(info, &tracks, config, true) {
            // Fetch lyrics
            if config.synced_lyrics || config.unsynced_lyrics {
                match self.lyrics(track.track_id.as_ref().unwrap()) {
                    Ok(lyrics) => track.lyrics = Some(lyrics),
                    Err(e) => warn!("Failed getting lyrics: {e}"),
                }
            }
            return Ok(Some((acc, track)));
        }
        Ok(None)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResultsResponse {
    pub results: SearchResults
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResults {
    // pub album: SearchResult<AlbumAttributes>,
    // pub artist: SearchResult<ArtistAttributes>,
    pub song: SearchResult<SongAttributes>
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult<I> {
    pub data: Vec<ItemMeta<I>>,
    pub group_id: String,
    pub name: String
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemMeta<A> {
    pub attributes: A,
    pub href: String,
    pub id: String,
    /// Lyric snippets and such
    pub meta: Option<Value>,
    pub relationships: Option<Relationships>,
}

impl Into<Track> for ItemMeta<SongAttributes> {
    fn into(self) -> Track {
        // Parse release date
        let mut release_year = None;
        let release_date = self.attributes.release_date.clone().map(|release_date| {
            if release_date.len() == 4 {
                release_year = release_date.parse().ok();
                None
            } else {
                NaiveDate::parse_from_str(&release_date, "%Y-%m-%d").ok()
            }
        }).flatten();
        // Get album
        let album = self.relationships.map(|r| r.albums.map(|a| a.data.first().map(|a| a.to_owned())).flatten()).flatten();

        // Create track
        Track {
            platform: "apple_music".to_string(),
            title: self.attributes.name,
            artists: vec![self.attributes.artist_name],
            album_artists: album.as_ref().map(|a| a.attributes.artist_name.to_string()).map(|a| vec![a]).unwrap_or(vec![]),
            album: Some(self.attributes.album_name),
            genres: self.attributes.genre_names,
            art: Some(self.attributes.artwork.url
                .replace("{w}", &self.attributes.artwork.width.to_string())
                .replace("{h}", &self.attributes.artwork.height.to_string())
                .replace("{f}", "png")
                .replace("{c}", "")),
            url: self.attributes.url,
            label: album.as_ref().map(|a| a.attributes.record_label.to_owned()).flatten(),
            catalog_number: Some(self.id.to_string()),
            track_id: Some(self.id),
            release_id: album.as_ref().map(|a| a.id.to_string()).unwrap_or(String::new()),
            duration: Duration::from_millis(self.attributes.duration_in_millis),
            track_number: Some(TrackNumber::Number(self.attributes.track_number)),
            track_total: album.as_ref().map(|a| a.attributes.track_count),
            disc_number: Some(self.attributes.disc_number as u16),
            isrc: Some(self.attributes.isrc),
            lyrics: None,
            release_year: release_year,
            release_date: release_date,
            ..Default::default()
        }
    }
}


#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SongAttributes {
    pub album_name: String,
    pub artist_name: String,
    pub artist_url: String,
    pub artwork: AppleMusicArtwork,
    pub audio_locale: String,
    pub composer_name: Option<String>,
    pub disc_number: i32,
    pub duration_in_millis: u64,
    pub genre_names: Vec<String>,
    pub has_lyrics: bool,
    pub has_time_synced_lyrics: bool,
    pub isrc: String,
    pub name: String,
    /// Can be year or NativeDate
    pub release_date: Option<String>,
    pub track_number: i32,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppleMusicArtwork {
    pub url: String,
    pub width: u64,
    pub height: u64
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Relationships {
    pub albums: Option<RelationshipWrap<AlbumAttributes>>,
    pub artists: Option<RelationshipWrap<ArtistAttributes>>
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelationshipWrap<D> {
    pub href: String,
    pub data: Vec<ItemMeta<D>>
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtistAttributes {
    pub url: String,
    pub name: String
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlbumAttributes {
    pub url: Option<String>,
    /// Can be year or NativeDate
    pub release_date: Option<String>,
    pub name: String,
    pub artist_name: String,
    pub artist_url: Option<String>,
    pub artwork: AppleMusicArtwork,
    pub record_label: Option<String>,
    pub track_count: u16,
    pub upc: String,
}

/// 1T source builder
pub struct AppleMusicBuilder {
    apple_music: Option<AppleMusic>
}

impl AutotaggerSourceBuilder for AppleMusicBuilder {
    fn new() -> Self {
        AppleMusicBuilder {
            apple_music: None
        }
    }

    fn get_source(&mut self, config: &TaggerConfig) -> Result<Box<dyn AutotaggerSource>, Box<dyn Error>> {
        // Already has instance
        if let Some(am) = self.apple_music.as_ref() {
            return Ok(Box::new(am.clone()));
        }
        // Create new
        let amc: AppleMusicConfig = serde_json::from_value(config.custom.get("apple_music").ok_or("Missing custom config")?.to_owned())?;
        let am = AppleMusic::new(&amc.media_user_token);
        // Chcek token
        am.fetch_token()?;
        self.apple_music = Some(am.clone());
        Ok(Box::new(am))
    }

    fn info(&self) -> PlatformInfo {
        PlatformInfo {
            id: "apple_music".to_string(),
            name: "Apple Music".to_string(),
            description: "Incl. album art up to 3000px, lyrics and more. Requires token".to_string(),
            version: "1.0.0".to_string(),
            icon: include_bytes!("icon.png"),
            max_threads: 4,
            requires_auth: true,
            supported_tags: supported_tags!(Title, Artist, AlbumArtist, Album, Genre, AlbumArt, URL, Label, CatalogNumber, TrackId, ReleaseId, Duration,
                TrackNumber, TrackTotal, DiscNumber, ISRC, ReleaseDate, SyncedLyrics, UnsyncedLyrics),
            custom_options: PlatformCustomOptions::new()
                .add("media_user_token", "Media User Token", PlatformCustomOptionValue::String { value: String::new(), hidden: Some(true) }),
        }
    }
}

#[derive(Deserialize)]
struct AppleMusicConfig {
    pub media_user_token: String   
}

onetagger_tagger::create_plugin!(AppleMusicBuilder, AppleMusic);
