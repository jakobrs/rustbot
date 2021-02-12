use crate::{api, commands::Args, Error};

use reqwest::header;
use serde::Deserialize;

const USER_AGENT: &str = "rust-lang/discord-mods-bot";

#[derive(Debug, Deserialize)]
struct Crates {
    crates: Vec<Crate>,
}
#[derive(Debug, Deserialize)]
struct Crate {
    id: String,
    name: String,
    #[serde(rename = "newest_version")]
    version: String,
    #[serde(rename = "updated_at")]
    updated: String,
    downloads: u64,
    #[serde(default)]
    description: String,
    documentation: Option<String>,
}

/// Queries the crates.io crates list and yields the first result, if any
fn get_crate(http: &reqwest::blocking::Client, query: &str) -> Result<Crate, Error> {
    info!("searching for crate `{}`", query);

    let crate_list = http
        .get("https://crates.io/api/v1/crates")
        .header(header::USER_AGENT, USER_AGENT)
        .query(&[("q", query)])
        .send()?
        .json::<Crates>()?;

    crate_list
        .crates
        .into_iter()
        .next()
        .ok_or(Error::NoCratesFound)
}

pub fn search(args: &Args) -> Result<(), Error> {
    let krate = get_crate(&args.http, args.body)?;
    args.msg.channel_id.send_message(&args.cx, |m| {
        m.embed(|e| {
            e.title(&krate.name)
                .url(format!("https://crates.io/crates/{}", krate.id))
                .description(&krate.description)
                .field("Version", &krate.version, true)
                .field("Downloads", &krate.downloads, true)
                .timestamp(krate.updated.as_str())
        });

        m
    })?;
    Ok(())
}

/// Provide the documentation link to an official Rust crate (e.g. std, alloc, nightly)
fn rustc_crate_link(crate_name: &str) -> Option<&str> {
    match crate_name.to_ascii_lowercase().as_str() {
        "std" => Some("https://doc.rust-lang.org/stable/std/"),
        "core" => Some("https://doc.rust-lang.org/stable/core/"),
        "alloc" => Some("https://doc.rust-lang.org/stable/alloc/"),
        "proc_macro" => Some("https://doc.rust-lang.org/stable/proc_macro/"),
        "beta" => Some("https://doc.rust-lang.org/beta/std/"),
        "nightly" => Some("https://doc.rust-lang.org/nightly/std/"),
        "rustc" => Some("https://doc.rust-lang.org/nightly/nightly-rustc/"),
        _ => None,
    }
}

pub fn doc_search(args: &Args) -> Result<(), Error> {
    let mut query_iter = args.body.splitn(2, "::");
    let crate_name = query_iter.next().unwrap();

    // The base docs url, e.g. `https://docs.rs/syn` or `https://doc.rust-lang.org/stable/std/`
    let mut doc_url = if let Some(rustc_crate) = rustc_crate_link(crate_name) {
        rustc_crate.to_string()
    } else {
        let krate = get_crate(&args.http, crate_name)?;
        let crate_name = krate.name;
        krate
            .documentation
            .unwrap_or_else(|| format!("https://docs.rs/{}", crate_name))
    };

    if let Some(item_path) = query_iter.next() {
        doc_url += "?search=";
        doc_url += item_path;
    }

    api::send_reply(args, &doc_url)?;

    Ok(())
}

/// Print the help message
pub fn help(args: &Args) -> Result<(), Error> {
    let help_string = "search for a crate on crates.io
```
?crate query...
```";
    api::send_reply(args, &help_string)?;
    Ok(())
}

/// Print the help message
pub fn doc_help(args: &Args) -> Result<(), Error> {
    let help_string = "retrieve documentation for a given crate
```
?docs crate_name...
```";
    api::send_reply(args, &help_string)?;
    Ok(())
}
