//! Several routers for one overlay network: i2pd HTTP proxies or SAM bridges
//! for I2P, nodes' FProxy for Hyphanet. Each reaches the whole network, so a
//! request goes to the best one that answers: the first in the order given
//! that is not resting. A router that could not be reached rests, longer each
//! time it fails again, and is tried last until it is back.

use std::future::Future;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use omnid_core::Network;
use tokio::task::JoinSet;
use url::Url;

use crate::{BoxError, FetchError};

/// How long a router rests after its first failure; each failure after that
/// doubles it, up to [`MAX_REST`].
const FIRST_REST: Duration = Duration::from_secs(15);
const MAX_REST: Duration = Duration::from_secs(600);
/// A router is on loopback or the owner's tailnet: if it has not accepted a
/// connection by now, it is not there (a tailnet address with Tailscale off
/// swallows the attempt instead of refusing it).
pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a stream waits on one router before also asking the next.
pub(crate) const STAGGER: Duration = Duration::from_secs(3);

/// A router, with what requests through it need: for HTTP, a proxy client
/// for i2pd or a plain one for FProxy; nothing for a SAM bridge.
pub(crate) struct Router<C = reqwest::Client> {
    pub(crate) url: Url,
    pub(crate) client: C,
    health: Mutex<Health>,
}

#[derive(Default)]
struct Health {
    failures: u32,
    resting_until: Option<Instant>,
}

impl<C> Router<C> {
    pub(crate) fn succeeded(&self) {
        *self.health.lock().expect("router lock poisoned") = Health::default();
    }

    pub(crate) fn failed(&self) {
        let mut health = self.health.lock().expect("router lock poisoned");
        health.failures = health.failures.saturating_add(1);
        let rest = FIRST_REST.saturating_mul(1 << (health.failures - 1).min(16)).min(MAX_REST);
        health.resting_until = Some(Instant::now() + rest);
    }

    /// When it is back, if it is resting now.
    fn resting_until(&self, now: Instant) -> Option<Instant> {
        self.health.lock().expect("router lock poisoned").resting_until.filter(|until| *until > now)
    }
}

pub(crate) struct Routers<C = reqwest::Client> {
    network: Network,
    routers: Vec<Router<C>>,
}

impl<C> Routers<C> {
    pub(crate) fn new(
        network: Network,
        urls: &[Url],
        client: impl Fn(&Url) -> Result<C, BoxError>,
    ) -> Result<Self, BoxError> {
        let routers = urls
            .iter()
            .map(|url| Ok(Router { url: url.clone(), client: client(url)?, health: Mutex::default() }))
            .collect::<Result<_, BoxError>>()?;
        Ok(Self { network, routers })
    }

    /// In the order given.
    pub(crate) fn all(&self) -> &[Router<C>] {
        &self.routers
    }

    pub(crate) fn urls(&self) -> impl Iterator<Item = &Url> {
        self.routers.iter().map(|r| &r.url)
    }

    /// Best first: the routers that are not resting, in the order given, then
    /// the resting ones, the soonest back first. Empty when none is configured.
    pub(crate) fn order(&self) -> Vec<&Router<C>> {
        let now = Instant::now();
        let mut order: Vec<(Option<Instant>, usize, &Router<C>)> =
            self.routers.iter().enumerate().map(|(i, r)| (r.resting_until(now), i, r)).collect();
        order.sort_by_key(|(resting, i, _)| (resting.is_some(), *resting, *i));
        order.into_iter().map(|(_, _, r)| r).collect()
    }

    /// The routers that are not resting, in the order given.
    pub(crate) fn awake(&self) -> Vec<&Router<C>> {
        let now = Instant::now();
        self.routers.iter().filter(|r| r.resting_until(now).is_none()).collect()
    }

    /// The best router's address, for links into it.
    pub(crate) fn best(&self) -> Option<&Url> {
        self.order().first().map(|r| &r.url)
    }

    /// Runs `attempt` on the routers, best first, until one succeeds. The next
    /// starts when the one before it fails, or after `stagger` while it is
    /// still trying, so a router that hangs costs seconds rather than the whole
    /// timeout. The first success wins and the attempts still running are
    /// dropped. A router that was unreachable rests; one that answered but
    /// could not reach the site did nothing wrong. With every attempt failed,
    /// the error is the best router's.
    pub(crate) async fn race<T, Fut>(&self, stagger: Duration, attempt: impl Fn(Url) -> Fut) -> Result<T, FetchError>
    where
        T: Send + 'static,
        Fut: Future<Output = Result<T, FetchError>> + Send + 'static,
    {
        let order = self.order();
        if order.is_empty() {
            return Err(FetchError::Unavailable { network: self.network, reason: "no router configured".into() });
        }
        let mut waiting = order.iter().enumerate().peekable();
        let mut attempts = JoinSet::new();
        let mut errors: Vec<(usize, FetchError)> = Vec::new();
        loop {
            if attempts.is_empty() {
                match waiting.next() {
                    Some((i, router)) => {
                        attempts.spawn(tagged(i, attempt(router.url.clone())));
                    }
                    None => break,
                }
            }
            tokio::select! {
                joined = attempts.join_next() => {
                    let (i, result) = match joined.expect("an attempt is running") {
                        Ok(tagged) => tagged,
                        Err(e) => return Err(FetchError::Transport { network: self.network, error: e.into() }),
                    };
                    match result {
                        Ok(value) => {
                            order[i].succeeded();
                            return Ok(value);
                        }
                        Err(error) => {
                            if is_unreachable(&error) {
                                order[i].failed();
                            }
                            errors.push((i, error));
                        }
                    }
                }
                () = tokio::time::sleep(stagger), if waiting.peek().is_some() => {
                    let (i, router) = waiting.next().expect("peeked");
                    attempts.spawn(tagged(i, attempt(router.url.clone())));
                }
            }
        }
        errors.sort_by_key(|(i, _)| *i);
        Err(errors.into_iter().next().expect("every router was tried").1)
    }
}

async fn tagged<T>(i: usize, attempt: impl Future<Output = Result<T, FetchError>>) -> (usize, Result<T, FetchError>) {
    (i, attempt.await)
}

/// Whether the router itself was not there, as opposed to answering that it
/// could not reach the site.
pub(crate) fn is_unreachable(error: &FetchError) -> bool {
    matches!(error, FetchError::Unavailable { .. } | FetchError::Timeout(_))
}

/// Whether this device may ask the router at `router` now: one at a tailnet
/// address only while this device is on a tailnet itself ([`crate::tailnet`]),
/// since outside one the address may be another machine's, and what goes to a
/// router is plain HTTP.
pub(crate) fn askable(network: Network, router: &Url) -> Result<(), FetchError> {
    if crate::tailnet::needs_tailnet(router) && !crate::tailnet::on_tailnet() {
        return Err(FetchError::Unavailable {
            network,
            reason: format!(
                "{router} is on a tailnet this device is not on now (is Tailscale on?); not asking it, in case \
                 another machine holds the address"
            ),
        });
    }
    Ok(())
}

/// A TCP connection to a router, given up after [`CONNECT_TIMEOUT`].
pub(crate) async fn connect(network: Network, router: &Url) -> Result<tokio::net::TcpStream, FetchError> {
    askable(network, router)?;
    let kind = if network == Network::Hyphanet { "node" } else { "router" };
    let unavailable =
        |what: &str| FetchError::Unavailable { network, reason: format!("{what} {router}; is the {kind} running?") };
    let (Some(host), Some(port)) = (router.host_str(), router.port_or_known_default()) else {
        return Err(FetchError::Unavailable { network, reason: format!("bad router address {router}") });
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    match tokio::time::timeout(CONNECT_TIMEOUT, tokio::net::TcpStream::connect((host, port))).await {
        Ok(Ok(stream)) => Ok(stream),
        Ok(Err(_)) => Err(unavailable("nothing is listening on")),
        Err(_) => Err(unavailable("no answer from")),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    fn routers(n: usize) -> Routers {
        let urls: Vec<Url> = (0..n).map(|i| format!("http://127.0.0.{}:4444", i + 1).parse().unwrap()).collect();
        Routers::new(Network::I2p, &urls, |_| Ok(reqwest::Client::new())).unwrap()
    }

    fn hosts(order: &[&Router]) -> Vec<String> {
        order.iter().map(|r| r.url.host_str().unwrap().to_owned()).collect()
    }

    #[test]
    fn a_router_that_failed_goes_last_until_it_is_back() {
        let routers = routers(3);
        assert_eq!(hosts(&routers.order()), ["127.0.0.1", "127.0.0.2", "127.0.0.3"]);
        routers.order()[0].failed();
        assert_eq!(hosts(&routers.order()), ["127.0.0.2", "127.0.0.3", "127.0.0.1"]);
        routers.order()[0].failed();
        assert_eq!(hosts(&routers.order()), ["127.0.0.3", "127.0.0.1", "127.0.0.2"], "the soonest back first");
        assert_eq!(hosts(&routers.awake()), ["127.0.0.3"], "the resting ones are left out");
        routers.routers[0].succeeded();
        assert_eq!(hosts(&routers.order()), ["127.0.0.1", "127.0.0.3", "127.0.0.2"]);
        assert_eq!(routers.best().unwrap().host_str(), Some("127.0.0.1"));
    }

    #[test]
    fn rests_longer_each_time_up_to_a_cap() {
        let routers = routers(1);
        let router = &routers.routers[0];
        let rest = || router.resting_until(Instant::now()).unwrap() - Instant::now();
        router.failed();
        assert!(rest() <= FIRST_REST && rest() > FIRST_REST / 2);
        router.failed();
        assert!(rest() > FIRST_REST && rest() <= FIRST_REST * 2);
        for _ in 0..40 {
            router.failed();
        }
        assert!(rest() <= MAX_REST && rest() > MAX_REST / 2);
    }

    fn down(network: Network) -> FetchError {
        FetchError::Unavailable { network, reason: "down".into() }
    }

    #[tokio::test]
    async fn the_next_router_answers_when_the_first_is_down() {
        let routers = routers(2);
        let won = routers
            .race(Duration::from_secs(60), |url| async move {
                match url.host_str() {
                    Some("127.0.0.1") => Err(down(Network::I2p)),
                    host => Ok(host.unwrap().to_owned()),
                }
            })
            .await
            .unwrap();
        assert_eq!(won, "127.0.0.2", "a failure starts the next at once, without the stagger");
        assert_eq!(hosts(&routers.order()), ["127.0.0.2", "127.0.0.1"], "the one that was down rests");
    }

    #[tokio::test]
    async fn a_router_that_hangs_is_raced_after_the_stagger() {
        let routers = routers(2);
        let started = Arc::new(AtomicUsize::new(0));
        let count = started.clone();
        let won = routers
            .race(Duration::from_millis(20), move |url| {
                count.fetch_add(1, Ordering::SeqCst);
                async move {
                    if url.host_str() == Some("127.0.0.1") {
                        std::future::pending::<()>().await;
                    }
                    Ok(url.host_str().unwrap().to_owned())
                }
            })
            .await
            .unwrap();
        assert_eq!(won, "127.0.0.2");
        assert_eq!(started.load(Ordering::SeqCst), 2);
        assert_eq!(hosts(&routers.order()), ["127.0.0.1", "127.0.0.2"], "slow is not down: nobody rests");
    }

    #[tokio::test]
    async fn the_best_routers_error_when_all_fail() {
        let routers = routers(2);
        let error = routers
            .race(Duration::from_secs(60), |url| async move {
                Err::<(), _>(FetchError::Transport {
                    network: Network::I2p,
                    error: format!("{} could not reach the site", url.host_str().unwrap()).into(),
                })
            })
            .await
            .unwrap_err();
        assert!(error.to_string().contains("127.0.0.1 could not"), "{error}");
        assert_eq!(hosts(&routers.order()), ["127.0.0.1", "127.0.0.2"], "they answered: nobody rests");

        let none: Routers = Routers::new(Network::Hyphanet, &[], |_| Ok(reqwest::Client::new())).unwrap();
        assert!(matches!(none.race(STAGGER, |_| async { Ok(()) }).await, Err(FetchError::Unavailable { .. })));
    }

    #[tokio::test]
    async fn a_router_that_is_not_listening_is_unavailable() {
        let error = connect(Network::I2p, &"http://127.0.0.1:9".parse().unwrap()).await.unwrap_err();
        assert!(is_unreachable(&error) && error.to_string().contains("is the router running"), "{error}");
    }
}
