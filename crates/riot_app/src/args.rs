//! Command-line arguments.

use std::path::PathBuf;

use riot_render::Lod;
use riot_sim::Overlay;

/// `--help` text.
pub const USAGE: &str = "\
riot_app — watch and influence a riot

USAGE: riot_app [OPTIONS]

Scenario:
  --people N         crowd size (default 3000)
  --squads S         police squads of 10 (default 8)
  --seed X           seed (default 1)
  --size M           district size in metres, up to 8192 (default 1024)
  --scenario PATH    RON scenario file (data/scenarios/*.ron)
  --tunables PATH    RON map of tunables { \"name\": value, ... }, hot-reloaded

Run:
  --ticks N          simulate N ticks headlessly before the first frame
  --speed K          game speed 1, 2, 4 or 8 (default 1)
  --paused           start paused
  --threads T        simulation threads (default: cores - 1)
  --demo             issue sample commands (gas, fire, supplies, escalation)
                     240 ticks before the end of --ticks

View:
  --camera X,Y,ZOOM  look at map point X,Y (m) at ZOOM pixels per metre
  --overlay NAME     none|density|fear|anger|threat|gas|messages|camera|pressure
  --lod NAME         auto|far|middle|close
  --window WxH       window size in pixels (default 1280x720)
  --no-markers       hide squad and entry markers
  --pixel N          pixel size: 1 (the art's own, default), 2 (twice as big),
                     0 (no pixel art: draw straight at the zoom)

Verification:
  --screenshot PATH  render after --ticks, save a PNG and exit
  --frames K         frames rendered (and timed) before the screenshot (default 30)
  --live SECONDS     after --ticks, run live at --speed for SECONDS, timing every
                     frame, then take the screenshot
";

#[derive(Clone, Debug)]
pub struct Args {
    pub people: u32,
    pub squads: u32,
    pub seed: u64,
    pub size: f32,
    pub scenario: Option<PathBuf>,
    pub tunables: Option<PathBuf>,
    pub ticks: u32,
    pub speed: u32,
    pub paused: bool,
    pub threads: Option<usize>,
    pub demo: bool,
    pub camera: Option<(f32, f32, f32)>,
    pub overlay: Overlay,
    pub lod: Option<Lod>,
    pub window: (i32, i32),
    pub markers: bool,
    /// 0 = no pixel art, else the pixel size.
    pub pixel: u32,
    pub screenshot: Option<PathBuf>,
    pub frames: u32,
    pub live: f32,
}

impl Default for Args {
    fn default() -> Self {
        Args {
            people: 3000,
            squads: 8,
            seed: 1,
            size: 1024.0,
            scenario: None,
            tunables: None,
            ticks: 0,
            speed: 1,
            paused: false,
            threads: None,
            demo: false,
            camera: None,
            overlay: Overlay::None,
            lod: None,
            window: (1280, 720),
            markers: true,
            pixel: 1,
            screenshot: None,
            frames: 30,
            live: 0.0,
        }
    }
}

fn num<T: std::str::FromStr>(flag: &str, v: Option<String>) -> Result<T, String> {
    let v = v.ok_or_else(|| format!("{flag} needs a value"))?;
    v.parse().map_err(|_| format!("{flag}: cannot parse `{v}`"))
}

/// An overlay by its `Overlay::name`.
pub fn parse_overlay(s: &str) -> Option<Overlay> {
    Overlay::ALL.into_iter().find(|o| o.name() == s)
}

impl Args {
    /// Parse arguments (without the program name).
    pub fn parse(it: impl IntoIterator<Item = String>) -> Result<Args, String> {
        let mut a = Args::default();
        let mut it = it.into_iter();
        while let Some(flag) = it.next() {
            match flag.as_str() {
                "--people" => a.people = num(&flag, it.next())?,
                "--squads" => a.squads = num(&flag, it.next())?,
                "--seed" => a.seed = num(&flag, it.next())?,
                "--size" => {
                    a.size = num::<f32>(&flag, it.next())?;
                    if !(128.0..=8192.0).contains(&a.size) {
                        return Err("--size must be between 128 and 8192".into());
                    }
                }
                "--scenario" => a.scenario = Some(num(&flag, it.next())?),
                "--tunables" => a.tunables = Some(num(&flag, it.next())?),
                "--ticks" => a.ticks = num(&flag, it.next())?,
                "--speed" => {
                    a.speed = num(&flag, it.next())?;
                    if ![1, 2, 4, 8].contains(&a.speed) {
                        return Err("--speed must be 1, 2, 4 or 8".into());
                    }
                }
                "--paused" => a.paused = true,
                "--threads" => a.threads = Some(num(&flag, it.next())?),
                "--demo" => a.demo = true,
                "--camera" => {
                    let v: String = num(&flag, it.next())?;
                    let p: Vec<f32> = v
                        .split(',')
                        .map(|s| s.trim().parse::<f32>())
                        .collect::<Result<_, _>>()
                        .map_err(|_| format!("--camera: cannot parse `{v}`"))?;
                    if p.len() != 3 || p[2] <= 0.0 {
                        return Err("--camera takes X,Y,ZOOM with ZOOM > 0".into());
                    }
                    a.camera = Some((p[0], p[1], p[2]));
                }
                "--overlay" => {
                    let v: String = num(&flag, it.next())?;
                    a.overlay =
                        parse_overlay(&v).ok_or_else(|| format!("--overlay: unknown `{v}`"))?;
                }
                "--lod" => {
                    let v: String = num(&flag, it.next())?;
                    a.lod = match v.as_str() {
                        "auto" => None,
                        "far" => Some(Lod::Far),
                        "middle" => Some(Lod::Middle),
                        "close" => Some(Lod::Close),
                        _ => return Err(format!("--lod: unknown `{v}`")),
                    };
                }
                "--window" => {
                    let v: String = num(&flag, it.next())?;
                    let (w, h) = v
                        .split_once('x')
                        .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
                        .ok_or_else(|| format!("--window: cannot parse `{v}`"))?;
                    a.window = (w, h);
                }
                "--no-markers" => a.markers = false,
                "--pixel" => {
                    a.pixel = num(&flag, it.next())?;
                    if a.pixel > 2 {
                        return Err("--pixel must be 0, 1 or 2".into());
                    }
                }
                "--screenshot" => a.screenshot = Some(num(&flag, it.next())?),
                "--frames" => a.frames = num::<u32>(&flag, it.next())?.max(1),
                "--live" => a.live = num::<f32>(&flag, it.next())?.max(0.0),
                "-h" | "--help" => return Err(String::new()),
                _ => return Err(format!("unknown argument `{flag}`")),
            }
        }
        Ok(a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Result<Args, String> {
        Args::parse(s.split_whitespace().map(String::from))
    }

    #[test]
    fn parses_the_verification_flags() {
        let a = parse(
            "--people 3000 --ticks 2400 --screenshot /tmp/s.png --camera 0,-5.5,4 --overlay fear",
        )
        .unwrap();
        assert_eq!(a.people, 3000);
        assert_eq!(a.ticks, 2400);
        assert_eq!(a.camera, Some((0.0, -5.5, 4.0)));
        assert_eq!(a.overlay, Overlay::Fear);
        assert_eq!(a.screenshot.unwrap().to_str(), Some("/tmp/s.png"));
    }

    #[test]
    fn rejects_bad_values() {
        assert!(parse("--speed 3").is_err());
        assert!(parse("--camera 1,2").is_err());
        assert!(parse("--bogus").is_err());
        assert!(parse("--size 9000").is_err());
        assert!(parse("--pixel 3").is_err());
        assert_eq!(parse("--pixel 0").unwrap().pixel, 0);
    }
}
