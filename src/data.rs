use std::collections::HashSet;

use crate::player::{Player, PlayerId};

pub fn load_players(path: &str) -> Result<Vec<Player>, csv::Error> {
    let mut reader = csv::Reader::from_path(path)?;

    let mut players = Vec::new();

    for result in reader.deserialize::<Player>() {
        let player = result?;
        players.push(player);
    }

    Ok(players)
}

#[derive(Debug, PartialEq)]
pub enum PlayerDataError {
    DuplicateId(PlayerId),
}

fn validate_unique_player_ids(players: &[Player]) -> Result<(), PlayerDataError> {
    let mut seen_ids = HashSet::new();
    for player in players {
        if !seen_ids.insert(player.id) {
            return Err(PlayerDataError::DuplicateId(player.id));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn duplicate_player_ids_are_rejected() {
        let players = vec![
            Player {
                id: PlayerId(0),
                name: String::from("Bird"),
                position: String::from("SF"),
                projected_value: 50,
            },
            Player {
                id: PlayerId(0),
                name: String::from("Larry"),
                position: String::from("SF"),
                projected_value: 50,
            },
        ];
        let result = validate_unique_player_ids(&players);
        assert_eq!(result, Err(PlayerDataError::DuplicateId(PlayerId(0))));
    }
    #[test]
    fn different_player_ids_are_accepted() {
        let players = vec![
            Player {
                id: PlayerId(0),
                name: String::from("Bird"),
                position: String::from("SF"),
                projected_value: 50,
            },
            Player {
                id: PlayerId(1),
                name: String::from("Larry"),
                position: String::from("SF"),
                projected_value: 50,
            },
        ];
        let result = validate_unique_player_ids(&players);
        assert_eq!(result, Ok(()));
    }
}
