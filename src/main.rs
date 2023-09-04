use async_recursion::async_recursion;
use clap::Parser;
use colored::*;
use reqwest::{Body, Client, Error, Response};
use select::{document::Document, predicate::Name};
use serde::{Deserialize, Serialize};
use std::{thread, time::Duration};
use terminal_hyperlink::Hyperlink;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiError {
    code: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MarketplaceQueryResponseItem {
    id: u64,
    name: String,
    product_id: u64,
    creator_type: String,
    creator_target_id: u64,
    price: Option<u32>,
    item_type: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MarketplaceQueryResponse {
    next_page_cursor: Option<String>,
    data: Option<Vec<MarketplaceQueryResponseItem>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AssetPurchaseQuery {
    expected_currency: u8,
    expected_price: u32,
    expected_seller_id: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AssetPurchaseResponse {
    errors: Option<Vec<ApiError>>,
}

impl Into<Body> for AssetPurchaseQuery {
    fn into(self) -> Body {
        let json_string =
            serde_json::to_string(&self).expect("Failed to serialize AssetPurchaseQuery");
        Body::from(json_string)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthenticatedUserResponse {
    id: u64,
}

#[derive(Parser)]
#[command(author, version, about)]
struct Args {
    /// Category of assets
    #[arg(short, long)]
    category: Option<String>,

    /// Subcategory of assets
    #[arg(short, long)]
    subcategory: Option<String>,

    /// .ROBLOSECURITY cookie to purchase assets
    #[arg(short, long)]
    auth: String,
}

fn get_search_url(args: &Args, next_page_cursor: &Option<String>) -> String {
    let category = args.category.clone();
    let subcategory = args.subcategory.clone();
    format!(
        "https://catalog.roblox.com/v2/search/items/details?category={}&subcategory={}&maxPrice=0&limit=120&cursor={}",
        category.unwrap_or("".to_string()), subcategory.unwrap_or("".to_string()), next_page_cursor.clone().unwrap_or("".to_string())
    )
}

async fn get_authenticated_user(
    client: &Client,
    auth: &String,
) -> Result<AuthenticatedUserResponse, Error> {
    client
        .get("https://users.roblox.com/v1/users/authenticated")
        .header("Cookie", format!(".ROBLOSECURITY={}", auth))
        .send()
        .await?
        .json::<AuthenticatedUserResponse>()
        .await
}

async fn authenticated_user_owns_bundle(
    client: &Client,
    auth: &String,
    item_id: u64,
) -> Result<bool, Box<dyn std::error::Error>> {
    let authenticated_user_id = get_authenticated_user(client, auth).await?.id;
    let user_owns_bundle = client
        .get(format!(
            "https://inventory.roblox.com/v1/users/{}/items/3/{}/is-owned",
            authenticated_user_id, item_id
        ))
        .header("Cookie", format!(".ROBLOSECURITY={}", auth))
        .send()
        .await?
        .json::<serde_json::Value>()
        .await?;

    Ok(user_owns_bundle.as_bool().unwrap())
}

async fn get_csrf_token(
    client: &Client,
    auth: &String,
) -> Result<String, Box<dyn std::error::Error>> {
    let body = client
        .get("https://www.roblox.com/home")
        .header("Cookie", format!(".ROBLOSECURITY={}", auth))
        .send()
        .await?
        .text()
        .await?;

    let document = Document::from_read(body.as_bytes()).unwrap();

    for node in document.find(Name("meta")) {
        if let Some(attr) = node.attr("name") {
            if attr == "csrf-token" {
                if let Some(csrf_value) = node.attr("data-token") {
                    return Ok(csrf_value.to_string());
                }
            }
        }
    }

    Ok(String::new())
}

async fn is_asset_available(
    client: &Client,
    auth: &String,
    asset: &MarketplaceQueryResponseItem,
) -> Result<bool, Box<dyn std::error::Error>> {
    if authenticated_user_owns_bundle(client, auth, asset.id).await? {
        return Ok(false);
    }

    if asset.creator_type == "User" && asset.creator_target_id == 1 {
        return Ok(false);
    }

    Ok(true)
}

async fn purchase_asset(
    client: &Client,
    asset: &MarketplaceQueryResponseItem,
    auth: &String,
    csrf_token: &String,
) -> Result<Response, Error> {
    client
        .post(format!(
            "https://economy.roblox.com/v1/purchases/products/{}",
            asset.product_id
        ))
        .body(AssetPurchaseQuery {
            expected_currency: 1,
            expected_price: 0,
            expected_seller_id: asset.creator_target_id,
        })
        .header("Content-Type", "application/json; charset=utf-8")
        .header("Cookie", format!(".ROBLOSECURITY={}", auth))
        .header("X-CSRF-TOKEN", csrf_token)
        .send()
        .await
}

#[async_recursion(?Send)]
async fn attempt_purchase(
    client: &Client,
    asset: &MarketplaceQueryResponseItem,
    args: &Args,
    csrf_token: &String,
    interval: Duration,
    ratelimit_interval: Duration,
) -> Result<(), Box<dyn std::error::Error>> {
    let asset_link = asset
        .name
        .hyperlink(format!("https://www.roblox.com/bundles/{}", asset.id));

    if asset.price.is_none() {
        println!("{} has no price", asset_link.truecolor(150, 150, 150));
        return Ok(());
    }

    if let Ok(purchase_response) = purchase_asset(client, asset, &args.auth, csrf_token).await {
        let purchase_body = purchase_response.json::<AssetPurchaseResponse>().await?;

        if purchase_body.errors.is_some() {
            for error in purchase_body.errors.unwrap().iter() {
                if error.code == 27 {
                    println!("{}", "Ratelimit reached. Waiting 65 seconds..".red());
                    thread::sleep(ratelimit_interval);
                } else {
                    println!("{} {}", "Failed to purchase".bold().red(), asset_link);
                }
            }

            attempt_purchase(
                client,
                asset,
                args,
                csrf_token,
                interval,
                ratelimit_interval,
            )
            .await?;

            return Ok(());
        }

        println!("{} {}", "Purchased".bold().green(), asset_link);
        thread::sleep(interval);
    } else {
        println!("{} {}", "Failed to purchase".bold().red(), asset_link);
        attempt_purchase(
            client,
            asset,
            args,
            csrf_token,
            interval,
            ratelimit_interval,
        )
        .await?;
        return Ok(());
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let client = Client::new();

    let csrf_token = get_csrf_token(&client, &args.auth).await?;

    let interval = Duration::from_secs(1);
    let ratelimit_interval = Duration::from_secs(65);

    let mut next_page_cursor: Option<String> = None;
    let mut purchased_items: u32 = 0;

    loop {
        let response = client
            .get(get_search_url(&args, &next_page_cursor))
            .send()
            .await?
            .json::<MarketplaceQueryResponse>()
            .await?;

        if response.data.is_none() {
            println!(
                "{} Bought {} items",
                "Done".bold().green(),
                purchased_items.to_string().bold().blue()
            );
            break;
        }

        for asset in response.data.unwrap().iter() {
            if is_asset_available(&client, &args.auth, asset).await? {
                attempt_purchase(
                    &client,
                    asset,
                    &args,
                    &csrf_token,
                    interval,
                    ratelimit_interval,
                )
                .await?;
                purchased_items += 1;
            }
        }

        if response.next_page_cursor.is_none() {
            println!(
                "{} Bought {} items",
                "Done".bold().green(),
                purchased_items.to_string().bold().blue()
            );
            break;
        }

        next_page_cursor = response.next_page_cursor;
    }

    Ok(())
}
