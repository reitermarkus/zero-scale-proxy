use a2s::info::{Info, ServerType, ServerOS, ExtendedServerInfo};

pub fn status_response() -> Info {
  Info {
    protocol: 0,
    name: "7 Days to Die".into(),
    map: "idle".into(),
    folder: "7DTD".into(),
    game: "7 Days To Die".into(),
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
