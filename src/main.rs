mod app;

use anyhow::Result;
use app::App;

fn main() -> Result<()> {
    let mut app = App::new();

    println!("Welcome to birdboard!");
    println!("Running: {}", app.running);
    app.quit();
    println!("Running: {}", app.running);

    Ok(())
}
