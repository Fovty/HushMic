//! The output latency of a `pw-cat` playback stream, measured while it
//! plays, for its [`PlayClock`]: from when the stream hands audio to the
//! graph until it is heard.
//!
//! PipeWire reports it on the stream's output port (the `Latency` param in
//! the input direction: so many quanta, plus frames, plus nanoseconds, as
//! the device's buffer and anything else downstream add up). The quanta are
//! the graph's current one, which `pw-dump` does not show but `pw-top`
//! does, for the driver the stream runs under.
//!
//! The device's part of that is the headroom PipeWire aims for, not what
//! its buffer holds: after the device was reopened (a quantum change does
//! that), PipeWire 1.0 was seen holding from a fifth of it to a quantum
//! more than it reported. So when the stream plays straight into an ALSA
//! device, its buffer is read from the kernel instead (`/proc/asound`, the
//! playback's `delay`), averaged over a few cycles: the latency is a
//! quantum (pw-cat fills its buffer for the next cycle) plus what the
//! device holds just before PipeWire writes, its average less half a
//! quantum.
//!
//! The device's buffer is read about twice a second (a few reads of a
//! file). `pw-top` attaches PipeWire's profiler to the graph and takes about
//! a second, so the quantum is read when the stream starts, again soon
//! after it changed or the clock had to start over (the graph changing its
//! quantum does that), and otherwise every [`TOP_EVERY`]. The port is read
//! every [`DUMP_EVERY`] (a different output device). A tool that is
//! missing or fails leaves the last value, or the clock's fallback: a
//! PipeWire whose `pw-top` lacks `-b` (0.3.48, Ubuntu 22.04) plays with
//! the port's figure at a quantum of 1024. The Flatpak has `pw-top` (its
//! runtime ships it, and the bundled PipeWire builds it with the SDK's
//! ncursesw).

use super::clock::PlayClock;
use crate::pipewire::tool_within;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// 48 kHz, frames per nanosecond.
const RATE: f64 = 48_000.0;
/// One `pw-dump` or `pw-top` call.
const BUDGET: Duration = Duration::from_secs(3);
/// The stream's node is looked for this long after pw-cat starts.
const FIND_FOR: Duration = Duration::from_secs(5);
/// One round: the device's buffer, and the latency handed to the clock.
const PACE: Duration = Duration::from_millis(500);
/// The quantum is read again this long after a reading, while it holds
/// still; [`PACE`] after it changed or the clock started over.
pub const TOP_EVERY: Duration = Duration::from_secs(2);
/// The port's latency is read again this often.
const DUMP_EVERY: Duration = Duration::from_secs(5);
/// The device's buffer is read this many times, this far apart, per
/// reading: a few cycles of its fill rising and falling.
const RING_READS: u32 = 24;
const RING_GAP: Duration = Duration::from_millis(10);

/// What the stream's port reports downstream of it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PortLatency {
    pub quanta: f64,
    pub frames: f64,
    pub ns: f64,
}

impl PortLatency {
    /// In frames at `quantum`.
    pub fn frames_at(&self, quantum: f64) -> f64 {
        self.quanta * quantum + self.frames + self.ns * RATE / 1e9
    }
}

/// Measures the latency of one playback stream while it lives: from
/// [`Watch::start`] until it is dropped (or the stream is gone).
pub struct Watch(Arc<AtomicBool>);

impl Watch {
    /// Measure the playback stream of `pid` (a pw-cat) into `clock`, on a
    /// thread of its own.
    pub fn start(pid: u32, clock: Arc<PlayClock>) -> Watch {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let _ = std::thread::Builder::new()
            .name("play-latency".into())
            .spawn(move || run(pid, &clock, &flag));
        Watch(stop)
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

fn run(pid: u32, clock: &PlayClock, stop: &AtomicBool) {
    let started = Instant::now();
    let found = loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        if let Some(f) = dump().as_deref().and_then(|d| stream_port(d, pid)) {
            break f;
        }
        if started.elapsed() > FIND_FOR {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    let (mut node, mut port) = found;
    let mut alsa = dump().as_deref().and_then(|d| alsa_device(d, node));
    let mut known: Option<f64> = None;
    // the device's last figure, and the quantum it was read at
    let mut last_ring: Option<(f64, f64)> = None;
    let mut next_dump = Instant::now() + DUMP_EVERY;
    let mut next_top = Instant::now();
    let mut anchors = clock.reanchors();
    while !stop.load(Ordering::Relaxed) {
        let began = Instant::now();
        if began >= next_dump {
            next_dump = began + DUMP_EVERY;
            match dump().as_deref().map(|d| (stream_port(d, pid), d)) {
                Some((Some(f), d)) => {
                    (node, port) = f;
                    let dev = alsa_device(d, node);
                    if dev != alsa {
                        // another device: the last one's figure isn't its
                        last_ring = None;
                    }
                    alsa = dev;
                }
                // the stream is gone
                Some((None, _)) => return,
                None => {}
            }
        }
        let started_over = clock.reanchors() != anchors;
        anchors = clock.reanchors();
        if started_over || began >= next_top {
            let quantum = tool_within("pw-top", &["-b", "-n", "2"], BUDGET)
                .filter(|(ok, _)| *ok)
                .and_then(|(_, out)| driver_quantum(&out, node));
            if stop.load(Ordering::Relaxed) {
                return;
            }
            let changed = quantum.is_some() && known.is_some() && quantum != known;
            // soon again after a change (it may still be settling), else
            // at the slow pace
            next_top = Instant::now() + if changed { PACE } else { TOP_EVERY };
            // a reading without the quantum (the graph changing it) keeps
            // the last one
            known = quantum.or(known);
        }
        let ring = match (&alsa, known) {
            (Some(dev), Some(q)) => match dev.latency(q, stop) {
                Some(l) => {
                    last_ring = Some((l, q));
                    Some(l)
                }
                // a reading that failed once (an xrun, the device being
                // reopened) keeps the last one at the same quantum: the
                // port's figure is a guess (VM 107: 90 ms short)
                None => last_ring.filter(|r| r.1 == q).map(|r| r.0),
            },
            _ => None,
        };
        match (ring, port, known) {
            (Some(l), _, q) => clock.set_latency(l, q),
            (None, Some(p), q) => clock.set_latency(p.frames_at(q.unwrap_or(1024.0)), q),
            // no latency on the port (old PipeWire): keep the fallback
            (None, None, q) => clock.set_latency(clock.latency(), q),
        }
        // a round at most every PACE
        while began.elapsed() < PACE && !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// The ALSA playback a stream plays straight into: its kernel status.
#[derive(Debug, PartialEq)]
pub struct AlsaDevice {
    pub status: PathBuf,
    pub hw_params: PathBuf,
}

impl AlsaDevice {
    /// The stream's latency (48 kHz frames) at the graph's `quantum`, from
    /// what the device's buffer holds over a few cycles. None when it is
    /// not running or cannot be read.
    fn latency(&self, quantum: f64, stop: &AtomicBool) -> Option<f64> {
        let rate = std::fs::read_to_string(&self.hw_params)
            .ok()
            .and_then(|t| device_rate(&t))?;
        let mut sum = 0.0;
        let mut got = 0u32;
        for _ in 0..RING_READS {
            if stop.load(Ordering::Relaxed) {
                return None;
            }
            if let Some(d) = std::fs::read_to_string(&self.status)
                .ok()
                .and_then(|t| ring_delay(&t))
            {
                sum += d;
                got += 1;
            }
            std::thread::sleep(RING_GAP);
        }
        (got >= RING_READS / 2).then(|| ring_latency(sum / got as f64, quantum, rate))
    }
}

/// The stream's latency (48 kHz frames) from the device's average `delay`
/// (its frames, at `rate`) and the graph's `quantum`: the quantum pw-cat
/// holds for the next cycle, plus the device's fill just before each
/// write (its average less half a quantum). Pure.
pub fn ring_latency(delay: f64, quantum: f64, rate: f64) -> f64 {
    (delay + quantum / 2.0) * RATE / rate
}

/// The ALSA playback device the stream `node` is linked straight to, from
/// a pw-dump (None for anything else: a filter, Bluetooth, a network
/// sink). Pure.
pub fn alsa_device(dump: &str, node: u32) -> Option<AlsaDevice> {
    let v: Value = serde_json::from_str(dump).ok()?;
    let objs = v.as_array()?;
    let mut targets = objs.iter().filter_map(|o| {
        let info = o.get("info")?;
        (o.get("type").and_then(Value::as_str) == Some("PipeWire:Interface:Link")
            && info.get("output-node-id").and_then(num) == Some(node as f64))
        .then(|| info.get("input-node-id").and_then(num))
        .flatten()
    });
    let target = targets.next()?;
    if targets.any(|t| t != target) {
        return None;
    }
    let p = objs
        .iter()
        .find(|o| o.get("id").and_then(num) == Some(target))?
        .get("info")?
        .get("props")?;
    if p.get("device.api").and_then(Value::as_str) != Some("alsa")
        || p.get("media.class").and_then(Value::as_str) != Some("Audio/Sink")
    {
        return None;
    }
    let card = p.get("alsa.card").and_then(num)? as u32;
    let device = p.get("alsa.device").and_then(num)? as u32;
    let sub = p.get("alsa.subdevice").and_then(num).unwrap_or(0.0) as u32;
    let dir = PathBuf::from(format!("/proc/asound/card{card}/pcm{device}p/sub{sub}"));
    Some(AlsaDevice {
        status: dir.join("status"),
        hw_params: dir.join("hw_params"),
    })
}

/// The `delay` of a running playback from its kernel status. Pure.
pub fn ring_delay(status: &str) -> Option<f64> {
    let mut running = false;
    let mut delay = None;
    for line in status.lines() {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        match k.trim() {
            "state" => running = v.trim() == "RUNNING",
            "delay" => delay = v.trim().parse::<f64>().ok(),
            _ => {}
        }
    }
    delay.filter(|_| running)
}

/// The rate of an open playback from its kernel `hw_params`. Pure.
pub fn device_rate(hw_params: &str) -> Option<f64> {
    hw_params.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        (k.trim() == "rate")
            .then(|| v.split_whitespace().next()?.parse::<f64>().ok())
            .flatten()
            .filter(|r| *r > 0.0)
    })
}

fn dump() -> Option<String> {
    crate::pipewire::pw_dump_within(BUDGET)
}

fn num(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

/// The playback stream node of process `pid` in a pw-dump, and the
/// latency its output ports report downstream (the largest, if they
/// differ; None when they report none). Pure.
pub fn stream_port(dump: &str, pid: u32) -> Option<(u32, Option<PortLatency>)> {
    let v: Value = serde_json::from_str(dump).ok()?;
    let objs = v.as_array()?;
    let of_type = |t: &'static str| {
        objs.iter()
            .filter(move |o| o.get("type").and_then(Value::as_str) == Some(t))
    };
    let props = |o: &Value| o.get("info").and_then(|i| i.get("props")).cloned();
    let client = of_type("PipeWire:Interface:Client").find_map(|c| {
        let p = props(c)?;
        (p.get("application.process.id").and_then(num) == Some(pid as f64))
            .then(|| c.get("id").and_then(num))
            .flatten()
    })?;
    let node = of_type("PipeWire:Interface:Node").find_map(|n| {
        let p = props(n)?;
        let playback = p.get("media.class").and_then(Value::as_str) == Some("Stream/Output/Audio");
        (playback && p.get("client.id").and_then(num) == Some(client))
            .then(|| n.get("id").and_then(num))
            .flatten()
    })? as u32;
    let mut best: Option<PortLatency> = None;
    for port in of_type("PipeWire:Interface:Port") {
        let Some(info) = port.get("info") else {
            continue;
        };
        let p = info.get("props").cloned().unwrap_or(Value::Null);
        if p.get("node.id").and_then(num) != Some(node as f64)
            || info.get("direction").and_then(Value::as_str) != Some("output")
        {
            continue;
        }
        let params = info
            .get("params")
            .and_then(|ps| ps.get("Latency"))
            .and_then(Value::as_array);
        for l in params.into_iter().flatten() {
            if l.get("direction").and_then(Value::as_str) != Some("Input") {
                continue;
            }
            let get = |k: &str| l.get(k).and_then(num).unwrap_or(0.0);
            let lat = PortLatency {
                quanta: get("minQuantum"),
                frames: get("minRate"),
                ns: get("minNs"),
            };
            if lat.quanta == 0.0 && lat.frames == 0.0 && lat.ns == 0.0 {
                continue;
            }
            if best.is_none_or(|b| lat.frames_at(1024.0) > b.frames_at(1024.0)) {
                best = Some(lat);
            }
        }
    }
    Some((node, best))
}

/// The quantum of the driver `node` runs under, from `pw-top -b`'s last
/// screen: the nearest driver line above the node's (a follower's name
/// comes after a `+` or `=`). None when the node is not listed or the
/// driver shows no quantum. Pure.
pub fn driver_quantum(top: &str, node: u32) -> Option<f64> {
    let last = top.rsplit_once("QUANT").map_or(top, |(_, rest)| rest);
    let mut driver: Option<f64> = None;
    for line in last.lines() {
        let t: Vec<&str> = line.split_whitespace().collect();
        if t.len() < 3 {
            continue;
        }
        let (Ok(id), Ok(quant)) = (t[1].parse::<u32>(), t[2].parse::<f64>()) else {
            continue;
        };
        let follower = t.iter().any(|w| *w == "+" || *w == "=");
        if !follower {
            driver = (quant > 0.0).then_some(quant);
        }
        if id == node {
            return if follower {
                driver
            } else {
                (quant > 0.0).then_some(quant)
            };
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// pw-top -b -n 2 on VM 107 during a replay (trimmed): the first screen
    /// has no quanta yet, the second has them.
    const TOP: &str = "\
S   ID  QUANT   RATE    WAIT    BUSY   W/Q   B/Q  ERR FORMAT           NAME
C   47      0      0    ---     ---   ---   ---     0                  alsa_output.pci-0000_08_0c.0.analog-stereo
C   81      0      0    ---     ---   ---   ---     0                  hushmic-miccheck
S   ID  QUANT   RATE    WAIT    BUSY   W/Q   B/Q  ERR FORMAT           NAME
I   30      0      0   0.0us   0.0us  ???   ???     0                  Dummy-Driver
R   47   2048  48000 308.5us  14.8us  0.03  0.00    0    S16LE 2 48000 alsa_output.pci-0000_08_0c.0.analog-stereo
R   66      0      0  25.2us   7.0us  0.00  0.00    0     F32P 1 48000  + hushmic_feed
R   81    960  48000  72.2us  23.4us  0.01  0.00    1    F32LE 1 48000  + hushmic-miccheck
R   99    480  48000  40.4us  12.1us  0.00  0.00    0    S16LE 1 48000  = pw-play
";

    #[test]
    fn the_quantum_is_the_driver_s_not_the_stream_s() {
        assert_eq!(driver_quantum(TOP, 81), Some(2048.0));
        assert_eq!(driver_quantum(TOP, 47), Some(2048.0));
        assert_eq!(driver_quantum(TOP, 5), None);
        assert_eq!(driver_quantum("", 81), None);
    }

    #[test]
    fn the_stream_is_found_by_its_process_and_its_ports_latency_read() {
        let dump = r#"[
          {"id": 80, "type": "PipeWire:Interface:Client",
           "info": {"props": {"application.process.id": 4983}}},
          {"id": 78, "type": "PipeWire:Interface:Client",
           "info": {"props": {"application.process.id": "777"}}},
          {"id": 82, "type": "PipeWire:Interface:Node",
           "info": {"props": {"client.id": 78, "media.class": "Stream/Input/Audio"}}},
          {"id": 81, "type": "PipeWire:Interface:Node",
           "info": {"props": {"client.id": 80, "media.class": "Stream/Output/Audio"}}},
          {"id": 85, "type": "PipeWire:Interface:Port",
           "info": {"direction": "output", "props": {"node.id": 81},
                    "params": {"Latency": [
                      {"direction": "Input", "minQuantum": 1.0, "maxQuantum": 1.0,
                       "minRate": 8192, "maxRate": 8192, "minNs": 0, "maxNs": 0},
                      {"direction": "Output", "minQuantum": 0.0, "maxQuantum": 0.0,
                       "minRate": 0, "maxRate": 0, "minNs": 0, "maxNs": 0}]}}},
          {"id": 88, "type": "PipeWire:Interface:Port",
           "info": {"direction": "input", "props": {"node.id": 82},
                    "params": {"Latency": [
                      {"direction": "Input", "minQuantum": 9.0, "minRate": 9, "minNs": 0}]}}}
        ]"#;
        let (node, lat) = stream_port(dump, 4983).unwrap();
        assert_eq!(node, 81);
        let lat = lat.unwrap();
        assert_eq!(
            lat,
            PortLatency {
                quanta: 1.0,
                frames: 8192.0,
                ns: 0.0
            }
        );
        assert_eq!(lat.frames_at(2048.0), 10_240.0);
        // a recorder of another process is not ours; nor a process we lack
        assert_eq!(stream_port(dump, 777), None);
        assert_eq!(stream_port(dump, 1), None);
    }

    #[test]
    fn the_device_is_the_alsa_sink_the_stream_plays_straight_into() {
        let dump = r#"[
          {"id": 47, "type": "PipeWire:Interface:Node",
           "info": {"props": {"media.class": "Audio/Sink", "device.api": "alsa",
                              "alsa.card": 1, "alsa.device": 0, "alsa.subdevice": 0}}},
          {"id": 60, "type": "PipeWire:Interface:Node",
           "info": {"props": {"media.class": "Audio/Sink", "node.name": "effects"}}},
          {"id": 90, "type": "PipeWire:Interface:Link",
           "info": {"output-node-id": 81, "input-node-id": 47}},
          {"id": 91, "type": "PipeWire:Interface:Link",
           "info": {"output-node-id": 81, "input-node-id": 47}},
          {"id": 92, "type": "PipeWire:Interface:Link",
           "info": {"output-node-id": 82, "input-node-id": 60}}
        ]"#;
        assert_eq!(
            alsa_device(dump, 81),
            Some(AlsaDevice {
                status: PathBuf::from("/proc/asound/card1/pcm0p/sub0/status"),
                hw_params: PathBuf::from("/proc/asound/card1/pcm0p/sub0/hw_params"),
            })
        );
        // through a filter: its latency is not the device's
        assert_eq!(alsa_device(dump, 82), None);
        assert_eq!(alsa_device(dump, 83), None);
    }

    #[test]
    fn the_device_s_buffer_is_read_while_it_runs() {
        let status = "state: RUNNING\nowner_pid   : 1569\ntrigger_time: 353.5\n\
                      tstamp      : 354.6\ndelay       : 2336\navail       : 30432\n";
        assert_eq!(ring_delay(status), Some(2336.0));
        assert_eq!(ring_delay("state: PREPARED\ndelay : 0\n"), None);
        assert_eq!(ring_delay("closed\n"), None);
        let hw = "access: MMAP_INTERLEAVED\nformat: S32_LE\nchannels: 2\nrate: 44100 (44100/1)\n";
        assert_eq!(device_rate(hw), Some(44_100.0));
        assert_eq!(device_rate("closed\n"), None);
        // 2590 frames before each write of a 480 quantum, 2830 on average
        assert_eq!(ring_latency(2_830.0, 480.0, 48_000.0), 3_070.0);
        assert_eq!(ring_latency(44_100.0, 0.0, 44_100.0), 48_000.0);
    }

    #[test]
    fn nanoseconds_count_as_frames() {
        let l = PortLatency {
            quanta: 0.0,
            frames: 0.0,
            ns: 10_000_000.0,
        };
        assert_eq!(l.frames_at(1024.0), 480.0);
    }
}
