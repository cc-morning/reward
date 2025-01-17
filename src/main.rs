use anyhow::Result;
use kuchiki::traits::TendrilSink;
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::{borrow::Borrow, collections::HashMap, io, ops::Add, time::Instant};

static DUNGEON_URL: &'static str = "https://github.com/EvanMeek/veloren-wecw-assets/tree/main/common/loot_tables/dungeon/";
static RAW_URL: &'static str = "https://cdn.jsdelivr.net/gh/EvanMeek/veloren-wecw-assets@main/common/loot_tables/dungeon/";
static TARGET_URL: &'static str = "https://cdn.jsdelivr.net/gh/EvanMeek/veloren-wecw-assets@main/";

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub enum LootSpec<T: AsRef<str>> {
    Item(T),
    ItemQuantity(T, u32, u32),
    LootTable(T),
    Nothing,
}

#[tokio::main]
async fn main() -> Result<()> {
    let tiers = get_tiers().await?;

    let mut ron_future = HashMap::with_capacity(tiers.len());
    let mut ron_cache = HashMap::with_capacity(tiers.len());
    let mut rate_cache = HashMap::<String, Vec<(String, Vec<(f32, f32, String)>)>>::with_capacity(tiers.len());

    let mut tier_str = String::new();
    for (index, tier) in tiers.iter().enumerate() {
        let key = if tier.contains("-") {
            let key = tier.split("-").collect::<Vec<&str>>()[1];
            format!("T{}", key.parse::<i32>().unwrap().add(1))
        } else {
            tier.clone()
        };

        tier_str.push_str(key.as_str());
        if index < tiers.len() - 1 {
            tier_str.push_str(", ");
        }

        let (key_clone, tier_clone) = (key.clone(), tier.clone());
        ron_future.insert(
            key_clone,
            tokio::spawn(async move {
                let rons = get_rons(tier_clone.as_str()).await.unwrap();
                return (tier_clone, rons);
            }),
        );
        ron_cache.insert(key, None);
    }

    loop {
        print!("\n{}: ", tier_str);
        io::Write::flush(&mut io::stdout()).expect("flush failed!");

        let choice = {
            let mut line = String::new();
            io::stdin().read_line(&mut line).unwrap();
            line
        };
        let choice = choice.trim();

        let now = Instant::now();

        let (tier, rons) = if let Some(join) = ron_future.get_mut(choice) {
            let value: (String, Vec<String>) = match join.await {
                Ok(value) => value,
                Err(_) => Default::default(),
            };
            ron_future.remove(choice);
            ron_cache.insert(choice.to_string(), Some(value.clone()));

            value
        } else {
            if let Some(Some(value)) = ron_cache.get(choice) {
                value.clone()
            } else {
                Default::default()
            }
        };

        let rate = if let Some(rate) = rate_cache.get(&tier) {
            rate.clone()
        } else {
            let rons = rons
                .par_iter()
                .map(|ron| {
                    let loots = match parse(&tier, &ron) {
                        Ok(loots) => loots,
                        Err(_) => Default::default(),
                    };
                    let weight: f32 = loots.iter().map(|loot| loot.0).sum();

                    let loots = loots
                        .par_iter()
                        .map(|loot| {
                            (
                                loot.0,
                                (loot.0 / weight) * 100.0,
                                parse_name(&loot.1).unwrap_or(String::from("无")),
                            )
                        })
                        .collect::<Vec<(f32, f32, String)>>();
                    (ron.clone(), loots)
                })
                .collect::<Vec<(String, Vec<(f32, f32, String)>)>>();
            rate_cache.insert(tier.clone(), rons.clone());

            rons
        };

        for ron in rate {
            println!("\n{}", ron.0);
            println!("{:<20}{:<30}{:<40}", "掉落权重", "掉率概率", "战利品");

            for loot in ron.1 {
                println!(
                    "{:<20}\t{:<30}\t{:<40}",
                    format!("{}", loot.0),
                    format!("{:.2}%", loot.1),
                    format!("  {}", loot.2)
                );
            }
        }
        println!("\ntime: {:.2}s", now.elapsed().as_secs_f32());
    }
}

async fn get_tiers() -> Result<Vec<String>> {
    get_files(DUNGEON_URL, "a[class=\"js-navigation-open Link--primary\"]").await
}

async fn get_rons(tier: &str) -> Result<Vec<String>> {
    let url = {
        let mut url = String::from(DUNGEON_URL);
        url.push_str(tier);
        url
    };

    get_files(&url, "a[title$=\".ron\"]").await
}

async fn get_files(url: &str, selectors: &str) -> Result<Vec<String>> {
    let body = reqwest::get(url).await?.text().await?;

    let document = kuchiki::parse_html().one(body);
    let r#as = document.select(selectors).unwrap();

    let rons = r#as
        .filter_map(|a| {
            let attrs = a.attributes.borrow();
            match attrs.borrow().get::<&str>("href") {
                Some(v) => {
                    let v = v.chars().rev().collect::<String>();
                    match v.find('/') {
                        Some(index) => Some(v[..index].chars().rev().collect::<String>()),
                        None => None,
                    }
                }
                _ => None,
            }
        })
        .collect::<Vec<String>>();

    Ok(rons)
}

fn parse(tier: &str, ron: &str) -> Result<Vec<(f32, LootSpec<String>)>> {
    let url = {
        let mut url = String::from(RAW_URL);
        url.push_str(tier);
        url.push_str("/");
        url.push_str(ron);
        url
    };

    let body = reqwest::blocking::get(url)?.text()?;
    let loots: Vec<(f32, LootSpec<String>)> = ron::de::from_str(body.as_str())?;

    Ok(loots)
}

fn parse_name(loot: &LootSpec<String>) -> Result<String> {
    let (url, range) = {
        let mut url = String::from(TARGET_URL);
        let (path, range) = match loot {
            LootSpec::Item(item) => (format!("{}", item), None),
            LootSpec::ItemQuantity(item, min, max) => (format!("{}", item), Some((min, max))),
            LootSpec::LootTable(_) => return Ok(String::from("道具包")),
            LootSpec::Nothing => return Ok(String::from("无")),
        };
        url.push_str(path.replace(".", "/").as_str());
        url.push_str(".ron");
        (url, range)
    };
    let body = reqwest::blocking::get(url)?.text()?;

    let regex = Regex::new(r#"".*?""#)?;
    let mut name = match regex.captures(&body) {
        Some(v) => v[0].replace("\"", ""),
        None => String::from("无"),
    };

    match range {
        Some(range) => {
            name.push_str(" (");
            name.push_str(range.0.to_string().as_str());
            name.push_str(" ~ ");
            name.push_str(range.1.to_string().as_str());
            name.push_str(")");
        }
        None => {}
    }

    Ok(name)
}
