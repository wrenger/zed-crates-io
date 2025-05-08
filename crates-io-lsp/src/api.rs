use anyhow::Result;
use reqwest::Client;
use serde::Deserialize;

pub async fn fetch_versions(
    name: &str,
    endpoint: &str,
    token: &str,
) -> Result<Vec<String>> {
    let prefix = if name.len() <= 2 {
        name.len().to_string()
    } else if name.len() == 3 {
        format!("{}/{}", name.len(), &name[0..1])
    } else {
        format!("{}/{}", &name[0..2], &name[2..4])
    };
    let mut request = Client::new().get(format!("{endpoint}/{prefix}/{name}"));
    if !token.is_empty() {
        request = request.bearer_auth(token);
    }

    let response = request.send().await?;
    let mut versions = Vec::new();
    for line in response.text().await?.lines() {
        let data: Version = serde_json::from_str(line)?;
        if !data.yanked {
            versions.push(data.vers);
        }
    }
    Ok(versions)
}

#[derive(Deserialize, Debug)]
struct Version {
    vers: String,
    yanked: bool,
}
