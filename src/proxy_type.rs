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

pub struct UnknownProxyType;

impl FromStr for ProxyType {
  type Err = UnknownProxyType;

  fn from_str(s: &str) -> Result<Self, Self::Err> {
    Ok(match s {
      "csgo" => ProxyType::Csgo,
      "sdtd" => ProxyType::Sdtd,
      "minecraft" => ProxyType::Minecraft,
      "teamspeak" => ProxyType::TeamSpeak,
      "generic" => ProxyType::Generic,
      _ => return Err(UnknownProxyType),
    })
  }
}
