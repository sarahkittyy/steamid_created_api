use actix_web::{web, App, HttpResponse, HttpServer, Responder};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sqlx::{query, query_as, MySqlPool};
use std::env;

#[derive(Deserialize)]
struct SteamIDQuery {
    steamid64: String,
}

#[derive(Serialize)]
struct SteamIDResponse {
    steamid64: String,
    timecreated: i64,
    error: i64,
}

#[derive(Serialize)]
struct EstimationResult {
    timecreated: i64,
    error: i64,
}

struct ApiData {
    pool: MySqlPool,
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    // Load environment variables
    dotenv::dotenv().ok();
    let database_url = env::var("DATABASE_URL").expect("DATABASE_URL must be set");

    // Connect to the database
    println!("Connecting to DB...");
    let pool = MySqlPool::connect(&database_url).await.unwrap();

    // Run the server
    let port: u16 = env::var("PORT")
        .expect("PORT must be set")
        .parse::<u16>()
        .expect("Could not parse PORT as u16");
    println!("Listening on {}", port);
    HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(ApiData { pool: pool.clone() }))
            .route("/lookup", web::get().to(lookup_steam_id))
    })
    .bind(("127.0.0.1", port))?
    .run()
    .await
}

async fn lookup_steam_id(
    data: web::Data<ApiData>,
    query: web::Query<SteamIDQuery>,
) -> impl Responder {
    let pool = data.pool.clone();
    let Ok(steamid64) = query.steamid64.parse::<i64>() else {
        return HttpResponse::BadRequest().body("Bad Steam ID.");
    };
    println!("Got request for {}", steamid64);
    let steam_api_key = env::var("STEAM_API_KEY").expect("STEAM_API_KEY must be set");

    // Check if the steamid64 is already cached
    if let Some(cached_time) = check_cache(&pool, steamid64).await {
        return HttpResponse::Ok().json(SteamIDResponse {
            steamid64: steamid64.to_string(),
            timecreated: cached_time,
            error: 0,
        });
    }

    // Query the Steam Web API
    let client = Client::new();
    let url = format!(
        "https://api.steampowered.com/ISteamUser/GetPlayerSummaries/v2/?key={}&steamids={}",
        steam_api_key, steamid64
    );

    let Ok(resp) = client.get(&url).send().await else {
        return HttpResponse::InternalServerError().body("Could not GET Steam API");
    };

    if resp.status().is_success() {
        let Ok(body) = resp.json::<serde_json::Value>().await else {
            return HttpResponse::InternalServerError().body("Steam API response is not JSON");
        };
        if let Some(timecreated) = body["response"]["players"][0]["timecreated"].as_i64() {
            // Cache the result
            cache_result(&pool, steamid64, timecreated).await;
            return HttpResponse::Ok().json(SteamIDResponse {
                steamid64: steamid64.to_string(),
                timecreated,
                error: 0,
            });
        }
    }

    // Estimate account age for private profiles
    let Some(estimation) = estimate_from_db(&pool, steamid64).await else {
        return HttpResponse::InternalServerError().body("Could not get DB estimation.");
    };
    HttpResponse::Ok().json(SteamIDResponse {
        steamid64: steamid64.to_string(),
        timecreated: estimation.timecreated,
        error: estimation.error,
    })
}

async fn check_cache(pool: &MySqlPool, steamid64: i64) -> Option<i64> {
    let result: Option<(i64,)> = query_as("SELECT timecreated FROM steam_ids WHERE steamid64 = ?")
        .bind(steamid64)
        .fetch_optional(pool)
        .await
        .unwrap();
    result.map(|(timecreated,)| timecreated)
}

async fn cache_result(pool: &MySqlPool, steamid64: i64, timecreated: i64) {
    query("INSERT INTO steam_ids (steamid64, timecreated) VALUES (?, ?)")
        .bind(steamid64)
        .bind(timecreated)
        .execute(pool)
        .await
        .unwrap();
}

async fn estimate_from_db(pool: &MySqlPool, steamid64: i64) -> Option<EstimationResult> {
    // SQL query to find the two closest steam IDs
    let closest: Vec<(i64, i64)> = query_as(
        "WITH closest_ids AS (
            SELECT
                steamid64,
                timecreated,
                ABS(steamid64 - ?) AS distance
            FROM steam_ids
            ORDER BY distance ASC
            LIMIT 2
        )
        SELECT steamid64, timecreated FROM closest_ids",
    )
    .bind(steamid64)
    .fetch_all(pool)
    .await
    .unwrap();

    // Ensure we got two closest IDs
    if closest.len() == 2 {
        let (lower_id, lower_time) = closest[0];
        let (upper_id, upper_time) = closest[1];

        // Local slope
        let local_slope = (upper_time - lower_time) as f64 / (upper_id - lower_id) as f64;
        println!(
            "Lower {} is {}, upper {} is {}\nSlope is {:.6}",
            lower_id, lower_time, upper_id, upper_time, local_slope
        );

        let estimated_time = lower_time + ((steamid64 - lower_id) as f64 * local_slope) as i64;
        // Calculate margin of error
        let margin_of_error =
            calculate_error_range(steamid64, lower_id, lower_time, upper_id, upper_time);

        return Some(EstimationResult {
            timecreated: estimated_time,
            error: margin_of_error,
        });
    }

    None
}

fn calculate_error_range(
    target_id: i64,
    lower_id: i64,
    lower_time: i64,
    upper_id: i64,
    upper_time: i64,
) -> i64 {
    // Local slope
    let local_slope = (upper_time - lower_time) as f64 / (upper_id - lower_id) as f64;

    // Difference from target ID
    let delta_id = (target_id - lower_id) as f64;

    // Estimate error as local slope * delta_id
    (local_slope * delta_id).abs() as i64
}
