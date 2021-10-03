use a2s::info::{Info, ServerType, ServerOS, ExtendedServerInfo};
use a2s::rules::Rule;

pub fn status_response(state: &str) -> Info {
  Info {
    protocol: 0,
    name: "7 Days to Die".into(),
    map: state.to_owned(),
    folder: "7DTD".into(),
    game: "7 Days to Die".into(),
    app_id: 0,
    players: 0,
    max_players: 0,
    bots: 0,
    server_type: ServerType::Dedicated,
    server_os: ServerOS::Linux,
    visibility: true,
    vac: false,
    the_ship: None,
    version: "".into(),
    edf: 0x80 | 0x10 | 0x20 | 0x01 | 0x40 * 0,
    extended_server_info: ExtendedServerInfo {
      port: Some(26902),
      steam_id: Some(90151620823146498),
      keywords: Some("AjxBAQAIAAOB0AESQYgBpAEePAQpHh4ABAQyugMAAwMDpAGkAaQBpAEFAAgtALMB".into()),
      game_id: Some(251570),
    },
    source_tv: None,
  }
}

pub fn rules_response(description: &str) -> Vec<Rule> {
  vec![
    Rule { name: "GameHost".into(),                    value: "7 Days to Die".into() },
    Rule { name: "GameName".into(),                    value: "World".into() },
    Rule { name: "ServerDescription".into(),           value: description.to_owned() },
    Rule { name: "ServerLoginConfirmationText".into(), value: "".into() },
    Rule { name: "ServerVisibility".into(),            value: "2".into() },
    Rule { name: "SteamID".into(),                     value: "90151742714337280".into() },
  ]
}
