//! FANZA's library API on `dlsoft.dmm.co.jp`, as its own pages use it: the works bought
//! (`/ajax/v1/library`, a page at a time) and how to download one
//! (`/ajax/v1/library/detail/single/`). Reading the answers is kept apart from asking.

use serde_json::Value;

use crate::shop::Work;

/// A file of a work's download: the path on `dlsoft.dmm.co.jp` that leads to it, and
/// the name it is saved under.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteFile {
    pub path: String,
    pub name: String,
}

/// The answer's `body`, or why FANZA refused.
fn body(json: &Value) -> Result<&Value, String> {
    match (&json["error"], &json["body"]) {
        (Value::Null, Value::Null) => Err("FANZA answered with nothing".into()),
        (Value::Null, body) => Ok(body),
        (error, _) => Err(format!(
            "FANZA refused: {}",
            error["errorCode"].as_str().unwrap_or("unknown error")
        )),
    }
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_owned()
}

/// The works on one page of `GET /ajax/v1/library` and how many there are in all.
pub fn library_page(json: &Value) -> Result<(usize, Vec<Work>), String> {
    let body = body(json)?;
    let total = body["totalCount"]
        .as_u64()
        .and_then(|n| usize::try_from(n).ok())
        .ok_or("FANZA's library has no count")?;
    let works = body["library"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let id = item["productId"].as_str()?;
            Some(Work {
                id: id.to_owned(),
                name: text(&item["title"]),
                maker: text(&item["brand"]["name"]),
                // `…ps.jpg` is the small package picture, `…pl.jpg` the large one.
                image: item["packageImageUrl"].as_str().map(|url| {
                    url.strip_suffix("ps.jpg")
                        .map_or_else(|| url.to_owned(), |stem| format!("{stem}pl.jpg"))
                }),
                kind: text(&item["libraryProductType"]),
                windows: true,
                version: text(&item["deliveryBeginDate"]),
            })
        })
        .collect();
    Ok((total, works))
}

/// `%XX` escapes decoded, as in a URL's query.
pub(super) fn percent_decoded(text: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(text.len());
    let mut rest = text.as_bytes();
    while let [first, tail @ ..] = rest {
        if *first == b'%' {
            let hex = std::str::from_utf8(tail.get(..2)?).ok()?;
            bytes.push(u8::from_str_radix(hex, 16).ok()?);
            rest = &tail[2..];
        } else {
            bytes.push(*first);
            rest = tail;
        }
    }
    String::from_utf8(bytes).ok()
}

/// A download link on `dlsoft.dmm.co.jp` with the name of the file it leads to: the
/// last part of its `filePath`, which must be a plain file name.
fn remote_file(path: &str) -> Result<RemoteFile, String> {
    let refuse = || format!("FANZA gave an unexpected download link: {path}");
    if !path.starts_with('/') || path.starts_with("//") {
        return Err(refuse());
    }
    let query = path
        .split_once('?')
        .map(|(_, query)| query)
        .ok_or_else(refuse)?;
    let file_path = query
        .split('&')
        .find_map(|pair| pair.strip_prefix("filePath="))
        .and_then(percent_decoded)
        .ok_or_else(refuse)?;
    let name = file_path.rsplit('/').next().unwrap_or_default();
    let plain =
        !name.is_empty() && name.chars().any(|c| c != '.') && !name.contains(['\\', ':', '\0']);
    if !plain {
        return Err(refuse());
    }
    Ok(RemoteFile {
        path: path.to_owned(),
        name: name.to_owned(),
    })
}

/// Whether a split part has an old RAR volume name (`.r00`, `.r01`, …).
fn old_volume(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(_, extension)| {
        extension.len() == 3
            && extension.starts_with(['r', 'R'])
            && extension[1..].bytes().all(|b| b.is_ascii_digit())
    })
}

/// The files to download for a work, from its `detail/single` answer, the one to
/// unpack first.
pub fn files(detail: &Value) -> Result<Vec<RemoteFile>, String> {
    let download = &body(detail)?["productDetail"]["download"];
    if download["canDownload"].as_bool() != Some(true) {
        return Err("FANZA does not let this work be downloaded".into());
    }
    if let Some(single) = download["singleFileUrl"].as_str() {
        return Ok(vec![remote_file(single)?]);
    }
    let combined = download["combinedFileUrl"]
        .as_str()
        .ok_or("FANZA gave no file to download")?;
    let mut first = remote_file(combined)?;
    let parts = download["splitFileUrlArray"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|part| {
            part.as_str()
                .map_or_else(|| Err("FANZA gave a broken part".into()), remote_file)
        })
        .collect::<Result<Vec<_>, String>>()?;
    // An old RAR's next volumes are found from the first one's name: give the
    // self-extracting first part the parts' name.
    if let Some(part) = parts.first().filter(|part| old_volume(&part.name))
        && let Some((stem, _)) = part.name.rsplit_once('.')
    {
        first.name = format!("{stem}.exe");
    }
    Ok(std::iter::once(first).chain(parts).collect())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{RemoteFile, files, library_page};
    use crate::shop::Work;

    #[test]
    fn reads_the_works_on_a_library_page() {
        let page = json!({ "error": null, "body": { "totalCount": 17, "library": [
            {
                "contentId": "alice_0024", "productId": "alice_0024",
                "deliveryBeginDate": "2014-04-25 10:00", "libraryProductType": "single",
                "floor": "Apcgame", "title": "ランス9 ヘルマン革命",
                "packageImageUrl": "https://pics.dmm.co.jp/digital/pcgame/alice_0024/alice_0024ps.jpg",
                "brand": { "name": "アリスソフト", "listUrl": "https://dlsoft.dmm.co.jp/list/?maker=30005" },
                "tagArray": [{ "code": "browser", "displayName": "ブラウザ対応" }]
            },
            {
                "productId": "set_0001", "deliveryBeginDate": "2020-01-01 00:00",
                "libraryProductType": "set", "title": "セット", "packageImageUrl": null,
                "brand": { "name": "メーカー" }
            }
        ] } });
        let (total, works) = library_page(&page).unwrap();
        assert_eq!(total, 17);
        assert_eq!(
            works,
            [
                Work {
                    id: "alice_0024".into(),
                    name: "ランス9 ヘルマン革命".into(),
                    maker: "アリスソフト".into(),
                    // The large package picture.
                    image: Some(
                        "https://pics.dmm.co.jp/digital/pcgame/alice_0024/alice_0024pl.jpg".into()
                    ),
                    kind: "single".into(),
                    windows: true,
                    version: "2014-04-25 10:00".into(),
                },
                Work {
                    id: "set_0001".into(),
                    name: "セット".into(),
                    maker: "メーカー".into(),
                    image: None,
                    kind: "set".into(),
                    windows: true,
                    version: "2020-01-01 00:00".into(),
                },
            ]
        );
        let past_the_end = json!({ "error": null, "body": { "totalCount": 17, "library": [] } });
        assert_eq!(library_page(&past_the_end).unwrap(), (17, vec![]));
        let refused = json!({ "error": { "errorCode": "E000-UNAUTHORIZED" }, "body": null });
        assert!(
            library_page(&refused)
                .unwrap_err()
                .contains("E000-UNAUTHORIZED")
        );
    }

    fn detail(download: &serde_json::Value) -> serde_json::Value {
        json!({ "error": null, "body": { "productDetail": {
            "product": { "productId": "x" },
            "download": download,
            "browser": null,
            "makerKeyNameArray": []
        } } })
    }

    fn url(file: &str, id: &str) -> String {
        format!(
            "/download/?filePath={}&productId={id}&floor=Apcgame",
            file.replace('/', "%2F")
        )
    }

    #[test]
    fn a_single_file_is_downloaded_as_it_is_named() {
        let zip = detail(&json!({
            "canDownload": true, "fileType": "zip", "volume": "162.75",
            "singleFileUrl": url("/bb/pcgame/selen_0004.zip", "selen_0004"),
            "combinedFileUrl": null, "splitFileUrlArray": []
        }));
        assert_eq!(
            files(&zip).unwrap(),
            [RemoteFile {
                path: url("/bb/pcgame/selen_0004.zip", "selen_0004"),
                name: "selen_0004.zip".into()
            }]
        );
    }

    #[test]
    fn a_split_archive_starts_with_its_combining_file() {
        // The old RAR volume names: the self-extracting first part takes the parts' name
        // so the next ones are found from it.
        let old = detail(&json!({
            "canDownload": true, "fileType": "rar2",
            "singleFileUrl": null,
            "combinedFileUrl": url("/bb/pcgame/alice_0024/alice_0024setup.exe", "alice_0024"),
            "splitFileUrlArray": [
                url("/bb/pcgame/alice_0024/alice_0024setup_.r00", "alice_0024"),
                url("/bb/pcgame/alice_0024/alice_0024setup_.r01", "alice_0024")
            ]
        }));
        let names: Vec<String> = files(&old).unwrap().into_iter().map(|f| f.name).collect();
        assert_eq!(
            names,
            [
                "alice_0024setup_.exe",
                "alice_0024setup_.r00",
                "alice_0024setup_.r01"
            ]
        );
        assert_eq!(
            files(&old).unwrap()[0].path,
            url("/bb/pcgame/alice_0024/alice_0024setup.exe", "alice_0024")
        );
        // `.partN.rar` names are found from `.part1.exe` as they are.
        let new = detail(&json!({
            "canDownload": true, "fileType": "rar",
            "combinedFileUrl": url("/bb/pcgame/alice_0053/alice_0053.part1.exe", "alice_0053"),
            "splitFileUrlArray": [url("/bb/pcgame/alice_0053/alice_0053.part2.rar", "alice_0053")]
        }));
        let names: Vec<String> = files(&new).unwrap().into_iter().map(|f| f.name).collect();
        assert_eq!(names, ["alice_0053.part1.exe", "alice_0053.part2.rar"]);
    }

    #[test]
    fn refuses_works_it_cannot_download_and_unsafe_names() {
        let closed = detail(&json!({ "canDownload": false, "singleFileUrl": null }));
        assert!(files(&closed).is_err());
        let nothing = detail(&json!({
            "canDownload": true, "singleFileUrl": null, "combinedFileUrl": null,
            "splitFileUrlArray": []
        }));
        assert!(files(&nothing).is_err());
        for path in [
            "/bb/pcgame/..",
            "/bb/pcgame/a%5Cb.zip",
            "/bb/pcgame/",
            "/bb/c%3A.zip",
        ] {
            let bad = detail(&json!({
                "canDownload": true,
                "singleFileUrl": format!("/download/?filePath={path}&productId=x")
            }));
            assert!(files(&bad).is_err(), "{path}");
        }
        let elsewhere = detail(&json!({
            "canDownload": true,
            "singleFileUrl": "https://example.com/download/?filePath=%2Fa.zip"
        }));
        assert!(files(&elsewhere).is_err());
    }
}
