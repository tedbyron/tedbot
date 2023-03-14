use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct AlbumSearch<'a> {
    #[serde(borrow)]
    pub results: Results<'a>,
}

#[derive(Debug, Deserialize)]
pub struct Results<'a> {
    #[serde(borrow)]
    pub albummatches: AlbumMatches<'a>,
}

#[derive(Debug, Deserialize)]
pub struct AlbumMatches<'a> {
    #[serde(borrow)]
    pub album: Vec<Album<'a>>,
}

#[derive(Debug, Deserialize)]
pub struct Album<'a> {
    pub name: &'a str,
    pub artist: &'a str,
    pub url: &'a str,
    #[serde(borrow)]
    pub image: Vec<Image<'a>>,
}

#[derive(Debug, Deserialize)]
pub struct Image<'a> {
    #[serde(rename = "#text")]
    pub text: &'a str,
    pub size: ImageSize,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageSize {
    Small,
    Medium,
    Large,
    ExtraLarge,
}
