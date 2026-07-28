use crate::player::Player;

pub fn load_players(path: &str) -> Result<Vec<Player>, csv::Error> {
    let mut reader = csv::Reader::from_path(path)?;

    let mut players = Vec::new();

    for result in reader.deserialize::<Player>() {
        let player = result?;
        players.push(player);
    }

    Ok(players)
}
