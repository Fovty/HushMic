//! The link from the DSP chain into the source host, off the main loop.
//!
//! `hushmic_source` lives in its own small host process that outlives the
//! chain, so apps recording HushMic stay on it through every chain restart
//! (they get silence while the chain is gone, never the raw mic). The
//! chain's output reaches it through one passive link the daemon makes.
//! Making it needs the new chain's ports, which appear only once the
//! session manager configured them, so it waits for them here, bounded,
//! and links by port id once exactly one chain output is listed (right
//! after a restart the dying chain's may still be). `pw-link` does not wait
//! for the session manager; a wedged PipeWire would, hence the caps.
//!
//! After a link the stream controls a mixer can change are put back:
//! WirePlumber restores a volume or mute a user once set on these streams
//! (pavucontrol lists them), and a muted internal stream silences HushMic.

use crate::pipewire::{self, CHAIN_INPUT, CHAIN_OUTPUT, CHAIN_OUT_PORT, HOST_FEED, HOST_IN_PORT};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// How long one tool call may take.
pub const CALL_CAP: Duration = Duration::from_secs(2);

/// How long a link job waits for the ports.
pub const LINK_LIMIT: Duration = Duration::from_secs(8);

/// What the worker is asked to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Job {
    /// Link the chain spawned under `generation` (a new one, or one whose
    /// link went missing) and put its stream controls back.
    Link { generation: u64 },
    /// Remove these links (by id).
    Unlink(Vec<u32>),
    /// Cut these links (id, from node, into node) where the id still joins
    /// those two nodes.
    Cut(Vec<(u32, String, String)>),
}

/// How a link job ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Linked and read back.
    Linked,
    /// The ports were not there (or not alone) within [`LINK_LIMIT`]:
    /// the session manager has not configured them yet. Not a failure.
    NoPorts,
    /// A newer chain was spawned meanwhile.
    Superseded,
    /// The link could not be made or read back.
    Failed(String),
}

/// A link job's outcome, with the chain generation it was for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Done {
    pub generation: u64,
    pub outcome: Outcome,
}

/// The port ids `pw-link -o -I` / `-i -I` lists for `port` (lines like
/// `  43 hushmic_dsp_out:output_MONO`). Pure.
pub fn port_ids(listing: &str, port: &str) -> Vec<u32> {
    listing
        .lines()
        .filter_map(|l| {
            let (id, name) = l.trim().split_once(char::is_whitespace)?;
            (name.trim() == port).then(|| id.parse().ok())?
        })
        .collect()
}

/// Whether `pw-link -l -I` lists a link from port `out` to port `inp`
/// (a port line `  43 a:out`, then its links `  48   |->   44 b:in`). Pure.
pub fn listed(listing: &str, out: u32, inp: u32) -> bool {
    let mut current = None;
    for l in listing.lines() {
        let t = l.trim();
        if let Some((_, rest)) = t.split_once("|->") {
            let peer = rest.split_whitespace().next().and_then(|x| x.parse().ok());
            if current == Some(out) && peer == Some(inp) {
                return true;
            }
        } else if !t.contains("|<-") {
            current = t
                .split_whitespace()
                .next()
                .and_then(|x| x.parse::<u32>().ok());
        }
    }
    false
}

/// The ids of the links `pw-link -l -I` lists at a port of one of `nodes`
/// (port lines `  43 node:port`, each followed by its links
/// `  48   |->   44 peer:port` or `  48   |<-   ...`). Pure.
pub fn links_at(listing: &str, nodes: &[&str]) -> Vec<u32> {
    let mut out = Vec::new();
    let mut ours = false;
    for l in listing.lines() {
        let t = l.trim();
        if t.contains("|->") || t.contains("|<-") {
            let id = t.split_whitespace().next().and_then(|x| x.parse().ok());
            if let (true, Some(id)) = (ours, id) {
                if !out.contains(&id) {
                    out.push(id);
                }
            }
        } else {
            let port = t.split_once(char::is_whitespace).map(|(_, p)| p.trim());
            ours = port
                .and_then(|p| p.split_once(':'))
                .is_some_and(|(node, _)| nodes.contains(&node));
        }
    }
    out
}

/// Every link `pw-link -l -I` lists, as (link id, from node, into node),
/// read from the output ports' sections (`  43 a:out`, then
/// `  48   |->   44 b:in`). Pure.
pub fn links_between(listing: &str) -> Vec<(u32, String, String)> {
    let mut out = Vec::new();
    let mut from: Option<String> = None;
    for l in listing.lines() {
        let t = l.trim();
        if let Some((id, rest)) = t.split_once("|->") {
            // `44 node name:port` (a node name may hold spaces)
            let peer = rest
                .trim()
                .split_once(char::is_whitespace)
                .and_then(|(_, p)| p.trim().rsplit_once(':'))
                .map(|(n, _)| n.to_string());
            if let (Ok(id), Some(f), Some(p)) = (id.trim().parse(), from.as_ref(), peer) {
                out.push((id, f.clone(), p));
            }
        } else if t.contains("|<-") {
        } else {
            from = t
                .split_once(char::is_whitespace)
                .and_then(|(_, p)| p.trim().rsplit_once(':'))
                .map(|(n, _)| n.to_string());
        }
    }
    out
}

/// The ids of the nodes named `name` in `pw-cli ls Node` (a block per
/// object: `id 41, type PipeWire:Interface:Node/3`, then its properties
/// such as `node.name = "hushmic_input"`). Pure.
pub fn node_ids(listing: &str, name: &str) -> Vec<u32> {
    let want = format!("node.name = \"{name}\"");
    let mut out = Vec::new();
    let mut current = None;
    for l in listing.lines() {
        let t = l.trim();
        if let Some(rest) = t.strip_prefix("id ") {
            current = rest.split(',').next().and_then(|x| x.trim().parse().ok());
        } else if t == want {
            if let Some(id) = current.take() {
                out.push(id);
            }
        }
    }
    out
}

/// One link job: blocking, every call capped, `current` read between
/// calls (a newer chain ends it).
fn link(generation: u64, current: &AtomicU64, limit: Duration) -> Outcome {
    let deadline = Instant::now() + limit;
    let list = |args: &[&str]| {
        pipewire::tool_within("pw-link", args, CALL_CAP)
            .filter(|(ok, _)| *ok)
            .map(|(_, out)| out)
    };
    let mut last = "the ports were not listed".to_string();
    loop {
        if current.load(Ordering::Acquire) != generation {
            return Outcome::Superseded;
        }
        let outs = list(&["-o", "-I"]).map(|l| port_ids(&l, CHAIN_OUT_PORT));
        let ins = list(&["-i", "-I"]).map(|l| port_ids(&l, HOST_IN_PORT));
        if let (Some([out]), Some([inp])) = (outs.as_deref(), ins.as_deref()) {
            let (out, inp) = (out.to_string(), inp.to_string());
            // Passive: the chain and the mic idle while nobody records.
            let made = pipewire::tool_within("pw-link", &["-P", &out, &inp], CALL_CAP);
            let read = list(&["-l", "-I"]);
            match (out.parse(), inp.parse(), read) {
                (Ok(o), Ok(i), Some(l)) if listed(&l, o, i) => {
                    reset_controls();
                    return Outcome::Linked;
                }
                _ => {
                    last = match made {
                        Some((true, _)) => "the link was made but not listed".into(),
                        Some((false, _)) => "pw-link refused the link".into(),
                        None => "pw-link did not answer in time".into(),
                    };
                }
            }
        }
        if Instant::now() + Duration::from_millis(100) >= deadline {
            return if last.starts_with("the ports") {
                Outcome::NoPorts
            } else {
                Outcome::Failed(last)
            };
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Put back what a mixer may have changed on the streams: the internal
/// pair at full volume and unmuted; the chain's capture only unmuted (its
/// volume is the user's knob for a hot mic). Best effort.
fn reset_controls() {
    let Some((true, nodes)) = pipewire::tool_within("pw-cli", &["ls", "Node"], CALL_CAP) else {
        return;
    };
    for (name, props) in [
        (
            CHAIN_OUTPUT,
            "{ mute = false, volume = 1.0, channelVolumes = [ 1.0 ] }",
        ),
        (
            HOST_FEED,
            "{ mute = false, volume = 1.0, channelVolumes = [ 1.0 ] }",
        ),
        (CHAIN_INPUT, "{ mute = false }"),
    ] {
        if let [id] = node_ids(&nodes, name)[..] {
            let id = id.to_string();
            let _ = pipewire::tool_within("pw-cli", &["set-param", &id, "Props", props], CALL_CAP);
        }
    }
}

#[derive(Default)]
struct Slot {
    /// Link jobs sent and not finished.
    busy: u32,
    last: Option<Done>,
    /// Link ids sent for removal and not handled yet.
    unlinking: Vec<u32>,
}

struct Shared {
    slot: Mutex<Slot>,
    cv: Condvar,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, Slot> {
        self.slot.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// What a link job's body is (the default runs [`link`]; tests swap it).
type Body = Box<dyn Fn(u64, &AtomicU64) -> Outcome + Send>;

/// The thread that runs link jobs, in order.
pub struct Worker {
    tx: mpsc::Sender<Job>,
    shared: Arc<Shared>,
    /// The chain generation that may be linked; others end Superseded.
    current: Arc<AtomicU64>,
}

impl Worker {
    /// `done` is called (on the worker thread) after each link job.
    pub fn spawn(done: Box<dyn Fn(Done) + Send>) -> Worker {
        Worker::with_body(done, Box::new(|g, c| link(g, c, LINK_LIMIT)))
    }

    fn with_body(done: Box<dyn Fn(Done) + Send>, body: Body) -> Worker {
        let (tx, rx) = mpsc::channel::<Job>();
        let shared = Arc::new(Shared {
            slot: Mutex::new(Slot::default()),
            cv: Condvar::new(),
        });
        let current = Arc::new(AtomicU64::new(0));
        let (s, c) = (shared.clone(), current.clone());
        let _ = std::thread::Builder::new()
            .name("hushmic-link".into())
            .spawn(move || {
                for job in rx {
                    match job {
                        Job::Link { generation } => {
                            let outcome = body(generation, &c);
                            let d = Done {
                                generation,
                                outcome,
                            };
                            {
                                let mut slot = s.lock();
                                slot.busy = slot.busy.saturating_sub(1);
                                slot.last = Some(d.clone());
                                s.cv.notify_all();
                            }
                            done(d);
                        }
                        Job::Unlink(ids) => {
                            // PipeWire reuses ids: by now one may name some
                            // other link. Only those still at the chain's
                            // output or the feed go.
                            let still = pipewire::tool_within("pw-link", &["-l", "-I"], CALL_CAP)
                                .filter(|(ok, _)| *ok)
                                .map(|(_, l)| links_at(&l, &[CHAIN_OUTPUT, HOST_FEED]))
                                .unwrap_or_default();
                            for id in ids.iter().filter(|id| still.contains(id)) {
                                let id = id.to_string();
                                let _ = pipewire::tool_within("pw-link", &["-d", &id], CALL_CAP);
                            }
                            s.lock().unlinking.retain(|id| !ids.contains(id));
                        }
                        Job::Cut(links) => {
                            // only while the id still joins the same two
                            // nodes (PipeWire reuses ids)
                            let now = pipewire::tool_within("pw-link", &["-l", "-I"], CALL_CAP)
                                .filter(|(ok, _)| *ok)
                                .map(|(_, l)| links_between(&l))
                                .unwrap_or_default();
                            for (id, _, _) in links.iter().filter(|c| now.contains(c)) {
                                let id = id.to_string();
                                let _ = pipewire::tool_within("pw-link", &["-d", &id], CALL_CAP);
                            }
                        }
                    }
                }
            });
        Worker {
            tx,
            shared,
            current,
        }
    }

    /// Link the chain spawned under `generation`; any job for an older one
    /// ends without linking.
    pub fn link(&self, generation: u64) {
        self.current.store(generation, Ordering::Release);
        self.shared.lock().busy += 1;
        if self.tx.send(Job::Link { generation }).is_err() {
            self.shared.lock().busy -= 1;
        }
    }

    /// A link job is out.
    pub fn busy(&self) -> bool {
        self.shared.lock().busy > 0
    }

    /// No chain may be linked any more (it was stopped).
    pub fn forget(&self) {
        self.current.store(0, Ordering::Release);
    }

    /// Remove these links (those already on their way are not sent again).
    pub fn unlink(&self, ids: Vec<u32>) {
        let ids: Vec<u32> = {
            let mut slot = self.shared.lock();
            let new: Vec<u32> = ids
                .into_iter()
                .filter(|id| !slot.unlinking.contains(id))
                .collect();
            slot.unlinking.extend(&new);
            new
        };
        if !ids.is_empty() && self.tx.send(Job::Unlink(ids.clone())).is_err() {
            self.shared.lock().unlinking.retain(|id| !ids.contains(id));
        }
    }

    /// Cut these links (id, from node, into node) where the id still joins
    /// those two nodes.
    pub fn cut(&self, links: Vec<(u32, String, String)>) {
        if !links.is_empty() {
            let _ = self.tx.send(Job::Cut(links));
        }
    }

    /// Wait (up to `limit`) for the outcome of the link job for
    /// `generation`.
    pub fn wait(&self, generation: u64, limit: Duration) -> Option<Outcome> {
        let slot = self.shared.lock();
        let slot = self
            .shared
            .cv
            .wait_timeout_while(slot, limit, |s| {
                s.last.as_ref().map(|d| d.generation) != Some(generation)
            })
            .unwrap_or_else(|e| e.into_inner())
            .0;
        slot.last
            .as_ref()
            .filter(|d| d.generation == generation)
            .map(|d| d.outcome.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OUTS: &str = "  43 hushmic_dsp_out:output_MONO\n  45 hushmic_feed:monitor_MONO\n  46 hushmic_source:capture_MONO\n";
    const LINKS: &str = "  43 hushmic_dsp_out:output_MONO\n  48   |->   44 hushmic_feed:input_MONO\n  44 hushmic_feed:input_MONO\n  48   |<-   43 hushmic_dsp_out:output_MONO\n";

    #[test]
    fn ports_and_links_are_read_from_pw_link_listings() {
        assert_eq!(port_ids(OUTS, "hushmic_dsp_out:output_MONO"), vec![43]);
        assert_eq!(port_ids(OUTS, "hushmic_feed:input_MONO"), Vec::<u32>::new());
        let two = format!("{OUTS}  51 hushmic_dsp_out:output_MONO\n");
        assert_eq!(port_ids(&two, "hushmic_dsp_out:output_MONO"), vec![43, 51]);
        assert!(listed(LINKS, 43, 44));
        assert!(!listed(LINKS, 44, 43), "direction counts");
        assert!(!listed(LINKS, 43, 45));
        assert!(!listed("", 43, 44));
    }

    #[test]
    fn links_are_found_at_the_internal_nodes_only() {
        let foreign = format!(
            "{LINKS}  60 alsa_input.mic:capture_FL\n  61   |->   62 obs:input_FL\n  63 hushmic_feed:input_MONO\n  64   |<-   65 obs:output_FL\n"
        );
        assert_eq!(
            links_at(&foreign, &["hushmic_dsp_out", "hushmic_feed"]),
            vec![48, 64]
        );
        assert_eq!(links_at(&foreign, &["hushmic_source"]), Vec::<u32>::new());
        assert_eq!(links_at("", &["hushmic_feed"]), Vec::<u32>::new());
    }

    #[test]
    fn node_ids_come_from_pw_cli_blocks() {
        let ls = "\tid 40, type PipeWire:Interface:Node/3\n \t\tclient.id = \"37\"\n \t\tnode.name = \"hushmic_dsp_out\"\n\tid 41, type PipeWire:Interface:Node/3\n \t\tnode.name = \"hushmic_feed\"\n\tid 42, type PipeWire:Interface:Node/3\n \t\tnode.description = \"x\"\n";
        assert_eq!(node_ids(ls, "hushmic_dsp_out"), vec![40]);
        assert_eq!(node_ids(ls, "hushmic_feed"), vec![41]);
        assert_eq!(node_ids(ls, "hushmic_input"), Vec::<u32>::new());
    }

    #[test]
    fn a_newer_chain_ends_the_older_job_and_wait_sees_its_own() {
        let (tx, rx) = mpsc::channel::<Done>();
        let tx = Mutex::new(tx);
        let w = Worker::with_body(
            Box::new(move |d| {
                let _ = tx.lock().unwrap().send(d);
            }),
            Box::new(|g, c| {
                // Waiting for ports until a newer chain comes along.
                let t = Instant::now();
                while c.load(Ordering::Acquire) == g && t.elapsed() < Duration::from_millis(300) {
                    std::thread::sleep(Duration::from_millis(5));
                }
                if c.load(Ordering::Acquire) != g {
                    Outcome::Superseded
                } else {
                    Outcome::Linked
                }
            }),
        );
        w.link(1);
        assert!(w.busy());
        std::thread::sleep(Duration::from_millis(30));
        w.link(2);
        let first = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(first.outcome, Outcome::Superseded);
        assert_eq!(w.wait(2, Duration::from_secs(5)), Some(Outcome::Linked));
        assert_eq!(w.wait(1, Duration::from_millis(50)), None);
        assert!(!w.busy());
    }
}
