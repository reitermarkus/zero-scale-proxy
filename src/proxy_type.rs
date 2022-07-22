use std::str::FromStr;

#[derive(Debug, Clone, Copy)]
pub enum ProxyType {
  Csgo,
  Sdtd,
  Minecraft,
  TeamSpeak,
  Generic,
}

impl Default for ProxyType {
  fn default() -> Self {
    Self::Generic
  }
}

impl ProxyType {
  pub fn as_str(&self) -> &str {
    match self {
      Self::Csgo => "csgo",
      Self::Sdtd => "sdtd",
      Self::Minecraft => "minecraft",
      Self::TeamSpeak => "teamspeak",
      Self::Generic => "generic",
    }
  }
}

pub struct UnknownProxyType;

impl FromStr for ProxyType {
  type Err = UnknownProxyType;

  fn from_str(s: &str) -> Result<Self, Self::Err> {
    Ok(match s {
      "csgo" => ProxyType::Csgo,
      "sdtd" | "7d2d" => ProxyType::Sdtd,
      "minecraft" => ProxyType::Minecraft,
      "teamspeak" => ProxyType::TeamSpeak,
      "generic" => ProxyType::Generic,
      _ => return Err(UnknownProxyType),
    })
  }
}
