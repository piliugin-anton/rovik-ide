//! Pages a command opens in a browser on a team server (D20): a dev server
//! that opens its page (`vite --open`, Create React App), a command line
//! tool's sign-in (`gh auth login --web`). In the user's private network
//! (network.rs) a command's browser is Rovik's: its shims run the daemon's
//! `open`, which asks over the network's socket ([`ask`]).
//!
//! A page of this user's own servers (page_url.rs) opens in the browser on
//! the user's computer, through a port of that computer forwarded here
//! (the daemon's [`Pages`]). Any other address is refused, for the user's
//! safety: a command must not open a page of its choosing on their
//! computer. Either way the program is told why, in words the agent reads
//! in the command's output.

use super::page_url::local_page;
use futures_util::future::BoxFuture;
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

/// The variable naming the socket the shims ask on.
pub const SOCKET_VAR: &str = "ROVIK_PAGES";

/// The variable naming the chat a command runs for.
pub const CHAT_VAR: &str = "ROVIK_CHAT";

/// The most pages a chat's commands may open a minute: a command that
/// loops on its browser opens no more tabs than this on the user's computer.
const MOST_A_MINUTE: usize = 5;

/// A page opened again this soon isn't opened twice.
const AGAIN_WITHIN: Duration = Duration::from_secs(10);

/// The longest a question's line may be.
const MOST_LINE: usize = 16 * 1024;

/// How long a program may take to say what it asks.
const ASK_WITHIN: Duration = Duration::from_secs(5);

/// How long [`ask`] waits for the answer: the daemon waits on the user's
/// computer for less than this.
const ANSWER_WITHIN: Duration = Duration::from_secs(60);

/// What opens a page in the browser on the computer a chat belongs to: the
/// daemon, through that computer's Rovik.
pub trait Pages: Send + Sync {
    /// Opens `url`, which a server on `port` of the network's loopback
    /// serves, for `chat`: what to tell the program that asked, or why it
    /// wasn't opened.
    fn open(
        &self,
        chat: String,
        url: String,
        port: u16,
    ) -> BoxFuture<'static, Result<String, String>>;
}

/// Answers the questions of the network's shims, on its socket at `path`
/// (network.rs), with `pages`: from now until the process ends. Within a
/// runtime.
pub fn serve(path: &Path, pages: Arc<dyn Pages>) -> std::io::Result<()> {
    let _ = std::fs::remove_file(path);
    let listener = tokio::net::UnixListener::bind(path)?;
    let recent = Arc::new(Mutex::new(Recent::default()));
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    tokio::spawn(answer(stream, pages.clone(), recent.clone()));
                }
                Err(e) => {
                    log::warn!("pages: {e}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
    });
    Ok(())
}

/// Reads one question, `<chat>\t<url>`, and answers it with a line:
/// `ok <what was done>` or `no <why not>`.
async fn answer(stream: tokio::net::UnixStream, pages: Arc<dyn Pages>, recent: Arc<Mutex<Recent>>) {
    let (read, mut write) = stream.into_split();
    let mut line = String::new();
    let mut read = tokio::io::BufReader::new(read).take(MOST_LINE as u64);
    let asked = tokio::time::timeout(ASK_WITHIN, read.read_line(&mut line)).await;
    let reply = match asked {
        Ok(Ok(_)) => decide(line.trim_end_matches(['\r', '\n']), &*pages, &recent).await,
        _ => Err("Rovik didn't hear which page to open.".to_string()),
    };
    let text = match reply {
        Ok(done) => format!("ok {}\n", one_line(&done)),
        Err(why) => format!("no {}\n", one_line(&why)),
    };
    let _ = write.write_all(text.as_bytes()).await;
    let _ = write.shutdown().await;
}

/// What becomes of the question `asked`: what was done, or why not.
async fn decide(asked: &str, pages: &dyn Pages, recent: &Mutex<Recent>) -> Result<String, String> {
    let (chat, url) = asked
        .split_once('\t')
        .filter(|(chat, _)| !chat.is_empty())
        .ok_or_else(|| {
            "Rovik doesn't know which chat this command runs for, so it can't open the page \
             on the user's computer."
                .to_string()
        })?;
    let port = local_page(url)?;
    let now = Instant::now();
    {
        let mut recent = recent.lock().unwrap_or_else(|e| e.into_inner());
        if recent.opened_lately(chat, url, now) {
            return Ok(format!(
                "Rovik opened {url} in the user's browser a moment ago; it isn't opened twice."
            ));
        }
        if !recent.may_ask(chat, now) {
            return Err(format!(
                "Rovik didn't open {url}: commands of this chat asked to open {MOST_A_MINUTE} \
                 pages within a minute already. Wait a minute, or tell the user the address."
            ));
        }
    }
    let opened = pages.open(chat.to_string(), url.to_string(), port).await;
    if opened.is_ok() {
        let mut recent = recent.lock().unwrap_or_else(|e| e.into_inner());
        recent.opened(chat, url, Instant::now());
    }
    opened
}

/// Asks the daemon, over `socket`, to open `url` for `chat`, as the shims
/// do: what it did, or why not.
pub fn ask(socket: &Path, chat: &str, url: &str) -> Result<String, String> {
    let unanswered = |e: std::io::Error| {
        format!(
            "Rovik couldn't be asked to open {url} ({}: {e}). Tell the user the address instead.",
            socket.display()
        )
    };
    let mut stream = std::os::unix::net::UnixStream::connect(socket).map_err(unanswered)?;
    stream
        .set_read_timeout(Some(ANSWER_WITHIN))
        .map_err(unanswered)?;
    let question = format!("{}\t{}\n", one_line(chat), one_line(url));
    stream.write_all(question.as_bytes()).map_err(unanswered)?;
    let mut line = String::new();
    std::io::BufReader::new(stream)
        .take(MOST_LINE as u64)
        .read_line(&mut line)
        .map_err(unanswered)?;
    let line = line.trim_end();
    match line.split_once(' ') {
        Some(("ok", done)) => Ok(done.to_string()),
        Some(("no", why)) => Err(why.to_string()),
        _ => Err(format!(
            "Rovik gave no answer about {url}. Tell the user the address instead."
        )),
    }
}

/// `text` on one line: a question or an answer is one.
fn one_line(text: &str) -> String {
    text.replace(['\r', '\n', '\t'], " ")
}

/// The pages each chat's commands asked for lately.
#[derive(Default)]
struct Recent {
    asked: HashMap<String, VecDeque<Instant>>,
    opened: HashMap<String, (String, Instant)>,
}

impl Recent {
    /// Whether `chat` may ask for another page at `now`: counts it.
    fn may_ask(&mut self, chat: &str, now: Instant) -> bool {
        let asked = self.asked.entry(chat.to_string()).or_default();
        while asked
            .front()
            .is_some_and(|&at| now.duration_since(at) > Duration::from_secs(60))
        {
            asked.pop_front();
        }
        if asked.len() >= MOST_A_MINUTE {
            return false;
        }
        asked.push_back(now);
        true
    }

    /// Whether `url` was opened for `chat` within [`AGAIN_WITHIN`] of `now`.
    fn opened_lately(&self, chat: &str, url: &str, now: Instant) -> bool {
        self.opened
            .get(chat)
            .is_some_and(|(last, at)| last == url && now.duration_since(*at) < AGAIN_WITHIN)
    }

    fn opened(&mut self, chat: &str, url: &str, at: Instant) {
        self.opened.insert(chat.to_string(), (url.to_string(), at));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chat_asks_for_a_few_pages_a_minute_and_the_same_page_once() {
        let mut recent = Recent::default();
        let start = Instant::now();
        for _ in 0..MOST_A_MINUTE {
            assert!(recent.may_ask("c", start));
        }
        assert!(!recent.may_ask("c", start), "over the limit");
        assert!(recent.may_ask("other", start), "each chat its own");
        assert!(
            recent.may_ask("c", start + Duration::from_secs(61)),
            "a minute on"
        );

        recent.opened("c", "http://localhost:1/", start);
        assert!(recent.opened_lately("c", "http://localhost:1/", start + Duration::from_secs(1)));
        assert!(!recent.opened_lately("c", "http://localhost:2/", start));
        assert!(!recent.opened_lately("c", "http://localhost:1/", start + AGAIN_WITHIN));
    }

    /// Opens every page it's asked for, saying so, and remembers them.
    #[derive(Default)]
    struct Opening(Mutex<Vec<(String, String, u16)>>);

    impl Pages for Opening {
        fn open(
            &self,
            chat: String,
            url: String,
            port: u16,
        ) -> BoxFuture<'static, Result<String, String>> {
            self.0.lock().unwrap().push((chat, url.clone(), port));
            Box::pin(async move { Ok(format!("opened {url}")) })
        }
    }

    #[test]
    fn a_question_is_answered_with_what_was_done_or_why_not() {
        crate::rt::block_on(async {
            let pages = Opening::default();
            let recent = Mutex::new(Recent::default());
            assert_eq!(
                decide("chat-1\thttp://localhost:5173/", &pages, &recent).await,
                Ok("opened http://localhost:5173/".to_string())
            );
            assert_eq!(
                pages.0.lock().unwrap().as_slice(),
                [(
                    "chat-1".to_string(),
                    "http://localhost:5173/".to_string(),
                    5173
                )]
            );
            let again = decide("chat-1\thttp://localhost:5173/", &pages, &recent).await;
            assert!(again.unwrap().contains("a moment ago"));
            assert_eq!(pages.0.lock().unwrap().len(), 1, "not opened twice");

            let refused = decide("chat-1\thttps://example.com/", &pages, &recent).await;
            assert!(refused.unwrap_err().contains("for the user's safety"));
            let no_chat = decide("\thttp://localhost:1/", &pages, &recent).await;
            assert!(no_chat.unwrap_err().contains("which chat"));
            assert_eq!(pages.0.lock().unwrap().len(), 1);
        });
    }

    #[test]
    fn the_shims_question_goes_over_the_socket_and_its_answer_comes_back() {
        let dir = crate::testing::temp_dir();
        let socket = dir.0.join("pages.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            for reply in ["ok opened it\n", "no not that one\n"] {
                let (stream, _) = listener.accept().unwrap();
                let mut line = String::new();
                std::io::BufReader::new(&stream)
                    .read_line(&mut line)
                    .unwrap();
                (&stream).write_all(reply.as_bytes()).unwrap();
                assert!(line.starts_with("chat-1\thttp://localhost:"), "{line}");
            }
        });
        assert_eq!(
            ask(&socket, "chat-1", "http://localhost:1/"),
            Ok("opened it".to_string())
        );
        assert_eq!(
            ask(&socket, "chat-1", "http://localhost:2/"),
            Err("not that one".to_string())
        );
        server.join().unwrap();
        let unreachable = ask(&dir.0.join("none.sock"), "c", "http://localhost:3/").unwrap_err();
        assert!(unreachable.contains("couldn't be asked"), "{unreachable}");
    }
}
