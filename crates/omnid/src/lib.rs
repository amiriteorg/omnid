//! The omnid daemon as a library: what `omnid serve` runs, and what the
//! Android app embeds.

pub mod api;
mod socks;

pub use socks::Access as ProxyAccess;

/// The app's browser's proxy: the password it wants, and where it listens.
pub struct BrowserProxy {
    pub password: String,
    /// A loopback listener the caller keeps open across daemons, so its port
    /// is never free while the browser may still send there: another app
    /// that took it could answer for the daemon and see, or send directly,
    /// everything the browser asks for. `None` binds a new loopback port.
    pub listener: Option<std::net::TcpListener>,
}

use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use omnid_core::{Magnet, Network, Source, now_secs};
use omnid_crawl::{
    CrawlConfig, CrawlControl, CrawlStore, Crawler, HARVESTED_TRUST, Job, SEED_TRUST, Safety, StatsSnapshot,
};
use omnid_dht::{Dht, DhtConfig};
use omnid_index::{SCHEMA_VERSION, SearchIndex};
use omnid_net::{FetchOptions, Gateway, GatewayConfig, RouterState};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

pub struct DaemonConfig {
    pub data_dir: PathBuf,
    /// Port 0 picks a free one; [`Daemon::addr`] reports it.
    pub listen: SocketAddr,
    pub gateway: GatewayConfig,
    pub crawl: CrawlConfig,
    pub safety: Safety,
    pub seeds: Vec<Job>,
    /// Required on every API request when set; see [`api::router`].
    pub api_token: Option<String>,
    pub start_paused: bool,
    /// Where to serve the proxy (SOCKS5 and HTTP on one port), if at all, and
    /// to whom. An open one has no authentication: anything that can reach it
    /// can use it, so keep it on loopback.
    pub socks: Option<(SocketAddr, ProxyAccess)>,
    /// Serves the app's built-in browser, on a loopback port
    /// ([`Daemon::browser_proxy_addr`]), to clients that send its password:
    /// everything through Tor or i2pd, never direct, each site on circuits of
    /// its own.
    pub browser_proxy: Option<BrowserProxy>,
    /// The BitTorrent DHT, for magnets' file lists and swarm sizes. `None`
    /// leaves magnets indexed from their links alone.
    pub dht: Option<DhtConfig>,
    /// Also index info hashes sampled from the whole DHT (BEP 51). This pulls
    /// in torrents nobody linked to, whatever they are; the safety gate's
    /// blocked terms are checked against their names and files.
    pub dht_harvest: bool,
    /// Keeps Ahmia's list of onion services reported for abuse in the safety
    /// gate: the last copy from the data directory at once, then fetched
    /// over Tor and refreshed daily.
    pub abuse_blocklist: bool,
}

/// Starter addresses on each overlay network, for a new index: services'
/// own onion sites and Ahmia's list of onion services (which leaves out
/// those reported for abuse), I2P's hubs and directories, the Hyphanet
/// project's default bookmarks, its indexes of freesites among them, and
/// IPFS's documentation and Wikipedia's snapshots on IPFS. Each answered on
/// 2026-11-21 (the IPFS ones through kubo 0.43.1 on 2026-11-22). A network's seeds wait while its router is down.
/// An onion site on Ahmia's blocklist is not added, though the seeds are
/// exempt from it: five older ones are on it wrongly. An eepsite is named
/// only if a new i2pd's address book has the name; otherwise it is given by
/// its b32 address, which every router resolves.
pub const DEFAULT_SEEDS: &[&str] = &[
    // Tor: the Tor Project, the press, privacy tools and their makers.
    "http://2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion/",
    "https://www.bbcweb3hytmzhn5d532owbu6oqadra5z3ar726vq5kgwwn6aucdccrad.onion/",
    "http://p53lf57qovyuvwsc6xnrppyply3vtqm7l6pcobkmyqsiofyeznfu5uqd.onion/",
    "http://archivep75mbjunhxc6x4j5mwjmomyxb573v42baldlqu56ruil2oiad.onion/",
    "http://sdolvtfhatvsysc6l34d65ymdwxcujausv7k5jk4cy5ttzhjoi6fzvyd.onion/",
    "http://vww6ybal4bd7szmgncyruucpgfkqahzddi37ktceo3ah7ngmcopnpyyd.onion/",
    "http://protonmailrmez3lotccipshtkleegetolb73fuirgj7r4o4vfu7ozyd.onion/",
    "http://o54hon2e2vj6c7m3aqqu6uyece65by3vgoxxhlqlsvkmacw6a7m7kiad.onion/",
    "http://qubesosfasa4zl44o4tws22di6kepyzfeqv3tg4e3ztknltfxqrymdad.onion/",
    "http://dds6qkxpwdeubwucdiaord2xgbbeyds25rbsgr73tbfpqpt4a6vjwsyd.onion/",
    "http://w5j6stm77zs6652pgsij4awcjeel3eco7kvipheu6mtr623eyyehj4yd.onion/",
    // The Tor Project's list of its onion services, its support and
    // community portals, and Tor Metrics.
    "http://xao2lxsmia2edq2n5zxg6uahx6xox2t7bfjw6b5vdzsxi7ezmqob6qid.onion/",
    "http://rzuwtpc4wb3xdzrj3yeajsvm3fkq4vbeubm2tdxaqruzzzgs5dwemlad.onion/",
    "http://xmrhfasfg5suueegrnc4gsgyi2tyclcy5oz7f5drnrodmdtob6t2ioyd.onion/",
    "http://hctxrvjzfpvmzh2jllqhgvvkoepxb4kfzdjm6h7egcwlumggtktiftid.onion/",
    // Debian's list of its onion services, its blog (Bits from Debian) and
    // Debian micronews.
    "http://jvgypgbnfyvfopg5msp6nwr2sl2fd6xmnguq35n7rfkw3yungjn2i4yd.onion/",
    "http://xpsuwzyljwcmk3vmurhw5ryvi66azdexzdabng5bqu56r6xc35puyeyd.onion/",
    "http://boowxjehkfc3ip6pnsrtt32s6j252rdzng5rcjhjt7yzh2ieiz5ka4yd.onion/",
    // Privacy Guides, the Whonix forum, Systemli (a tech collective) and
    // Nos oignons (Tor exit relays run by donations).
    "http://www.xoe4vn5uwdztif6goazfbmogh6wh5jc4up35bqdflu6bkdc5cas5vjqd.onion/",
    "http://forums.dds6qkxpwdeubwucdiaord2xgbbeyds25rbsgr73tbfpqpt4a6vjwsyd.onion/",
    "http://7sk2kov2xwx6cbc32phynrifegg6pklmzs7luwcggtzrnlsolxxuyfyd.onion/",
    "http://oignonspeuuqi7ovpov7srtuhpqut2vgt7lcmenfwwvjgjnbbsx4qeyd.onion/",
    // Amnesty International and Bellingcat.
    "https://www.amnestyl337aduwuvpf57irfl54ggtnuera45ygcxzuftwxjvvmpuzqd.onion/",
    "http://bellcatmbguthn3age23lrbseln2lryzv3mt7whis7ktjw4qrestbzad.onion/",
    // Free software: Cwtch, OnionShare, F-Droid and Bitcoin Core.
    "http://cwtchim3z2gdsyb27acfc26lup5aqbegjrjsqulzrnkuoalq5h4gmcid.onion/",
    "http://lldan5gahapx5k7iafb3s4ikijc4ni7gx5iywdflkba5y2ezyg6sjgyd.onion/",
    "http://fdroidorg6cooksyluodepej4erfctzk7rrjpjbbr6wx24jh3lqyfwyd.onion/",
    "http://6hasakffvppilxgehrswmffqurlcjjjhd76jgvaqmsg6ul25s7t3rzyd.onion/",
    // Ahmia, a search engine for onion services, and its list of them.
    "http://juhanurmihxlp77nkq76byazcldy2hlmovfu2epvl5ankdibsot4csyd.onion/",
    "http://juhanurmihxlp77nkq76byazcldy2hlmovfu2epvl5ankdibsot4csyd.onion/onions/",
    // The Tor Project's specifications, research and Snowflake sites, and
    // OnionShare's documentation.
    "http://i3xi5qxvbrngh3g6o7czwjfxwjzigook7zxzjmgwg5b7xnjcn5hzciad.onion/",
    "http://xhqthou6scpfnwjyzc3ekdgcbvj76ccgyjyxp6cgypxjlcuhnxiktnqd.onion/",
    "http://oljlphash3bpqtrvqpr5gwzrhroziw4mddidi5d2qa4qjejcbrmoypqd.onion/",
    "http://docs.lldan5gahapx5k7iafb3s4ikijc4ni7gx5iywdflkba5y2ezyg6sjgyd.onion/2.7.2/en/",
    // MetaGer (a search engine), FragDenStaat (freedom-of-information
    // requests in Germany), the Monero documentation, Wasabi Wallet, AirVPN
    // and Njalla.
    "http://metagerv65pwclop2rsfzg4jwowpavpwd6grhhlvdgsswvo6ii4akgyd.onion/",
    "http://fdstaat23zv6kdmntgkvdzkr7hipl5oqswwi3xawzkj2w2gwsbxmrwyd.onion/",
    "http://xmrdoc6phnvjbf5hmjbwdfu47zavzfngymlnwhs2gyxxpxmad4c65kyd.onion/",
    "http://wasabiukrxmkdgve5kynjztuovbg43uxcbcxn6y2okcrsg7gb6jdmbad.onion/",
    "https://airvpn3epnw2fnsbx5x2ppzjs6vxtdarldas7wjyqvhscj7x43fxylqd.onion/",
    "https://njallalafimoej5i4eg7vlnqjvmb6zhdh27qxcatdn647jtwwwui3nad.onion/",
    // I2P: the project, its news, forum and registries, and directories.
    "http://i2p-projekt.i2p/",
    "http://planet.i2p/",
    "http://zzz.i2p/",
    "http://i2pforum.i2p/",
    "http://reg.i2p/",
    "http://inr.i2p/",
    "http://stats.i2p/",
    "http://notbob.i2p/",
    "http://isitup.i2p/",
    // The I2P wiki, the Russian community's wiki (I2P-ilita), I2P+, I2P's
    // Git forge and Postman's mail service. All are in the address book a
    // new i2pd starts with: other names do not resolve until it has more.
    "http://wiki.i2p-projekt.i2p/wiki/",
    "http://wiki.ilita.i2p/",
    "http://skank.i2p/",
    "http://git.idk.i2p/",
    "http://hq.postman.i2p/",
    // By b32 address, from reg.i2p's hosts.txt: Let's Decentralize (a guide
    // to the overlay networks, with lists of their sites), the discuss.i2p
    // forum, Shadow Wiki (privacy and surveillance), the Anthrill Linux wiki,
    // The Cypherpunk Handbook and the I2P Learning Lab.
    "http://mqtlargpv4247iylywxw6ibi6qpz6my5duqm33c4lcdhjg5yfh7q.b32.i2p/", // letsdecentralize.i2p
    "http://wrnowoalzz5mquzaygmcrm4feuqco5gsxyn42fizbqjaprtdx5gq.b32.i2p/", // discuss.i2p
    "http://63gxkfc4hlcbxrdoepw2i2hyxai5qkxmi636ag3y7sf5tq3imoya.b32.i2p/", // shadowwiki.i2p
    "http://x3yq7si6yhwjuo7h67h5dn2odoncu2jdkj3wzkttycar5dq2yvcq.b32.i2p/", // wiki.anthrill.i2p
    "http://poy7mv4bgjnjgxq73hck72x5lnspvev4pnsiea2gdyegt7xpeg3q.b32.i2p/", // cypherpunk-handbook.i2p
    "http://7s6q2f2mmnf7nhryd75ivy4dzviw2fuqizsligzglvj7wkchp5nq.b32.i2p/", // theinformed.i2p
    // i2pd's maintainers acetone and R4SAS (with his list of services on
    // i2pd), the I2P plugin repository, Free Software Fans (a Chinese free
    // software community), the OS3 security and network engineering
    // master's site and ArchWorks (a self-hosting blog).
    "http://tzwfy3dnqtm4wuofmcp3gcb5qjcytri635ei7kw7yrl6n3ul5n2a.b32.i2p/", // acetone.i2p
    "http://2gafixvoztrndawkmhfxamci5lgd3urwnilxqmlo6ittu552cndq.b32.i2p/", // r4sas.i2p
    "http://xowts5nmf5job77cqkngrwrmmud4suolt7tzqadmzhqhwew3whkq.b32.i2p/", // plugins.i2p
    "http://fsfansa6cam2xdnbhekn6jirbijkgnve33pvf5vd6u5md6pqjjka.b32.i2p/", // fsfans.i2p
    "http://7ugh7a46mrcnhe42k3nokjncbvwtv2fkpxlvd5c6odlfdb7puaqq.b32.i2p/", // os3.i2p
    "http://yvbkgfldebduben772l4ngcrxassjykeamuskljqktpwrpqoqg3q.b32.i2p/", // archworks.i2p
    // Software on I2P: EasyGPG, Xeres (friend-to-friend messaging), the
    // Kestrel messaging protocol and Revuo Monero (a Monero newsletter);
    // and Programthink's blog (Chinese, on privacy and censorship).
    "http://bwxry5alzx5ihgrd3glah4eotddblzhalvpheppnw4zcajzqoora.b32.i2p/", // easygpg2.i2p
    "http://f7vdjudujlxlvn6xru2tdllvzyejavmb27r7ytatjszrumpxlt4a.b32.i2p/", // xeres.i2p
    "http://kestrellnbvhitrqntpk3tairex65bvmmtybbtuzh3k5z3sdtafa.b32.i2p/", // kestrel.i2p
    "http://qve7i3ke5h2mbbyphd7agmh5awzcwulvrvghjvhndkagdbi4mkcq.b32.i2p/", // revuo-xmr.i2p
    "http://programygfltn24q6foacm5gbuo3z2geub2k2k366llx3bph4ula.b32.i2p/", // programthink.i2p
    // Hyphanet: the node's default bookmarks, Clean Spider (an index of
    // freesites) and the Index of Indexes among them.
    "freenet:USK@62H8KFSZWMyQ2MQgwvNhEYJ2m3SQl696PfsVfWQ-HQo,cJrPvdNz4AnrHJQXNteDV7k3YnAVY-MClt84gwH2qEo,AQACAAE/freenet-first-steps/24/",
    "freenet:USK@5ijbfKSJ4kPZTRDzq363CHteEUiSZjrO-E36vbHvnIU,ZEZqPXeuYiyokY2r0wkhJr5cy7KBH9omkuWDqSC6PLs,AQACAAE/clean-spider/547/",
    "freenet:USK@t-YmshUm43nS16QFieoZMfBdfieb9Opy0a2aMWsoH8o,7NqDX88r1R4SC~elmpW4SuKbnAOka~MKUuBufUWa2xQ,AQACAAE/jFniki.Index/21/",
    "freenet:USK@0iU87PXyodL2nm6kCpmYntsteViIbMwlJE~wlqIVvZ0,nenxGvjXDElX5RIZxMvwSnOtRzUKJYjoXEDgkhY6Ljw,AQACAAE/freenetproject-mirror/519/",
    "freenet:USK@t5zaONbYd5DvGNNSokVnDCdrIEytn9U5SSD~pYF0RTE,guWyS9aCMcywU5PFBrKsMiXs7LzwKfQlGSRi17fpffc,AQACAAE/fsng/58/",
    "freenet:USK@8r-uSRcJPkAr-3v3YJR16OCx~lyV2XOKsiG4MOQQBMM,P42IgNemestUdaI7T6z3Og6P-Hi7g9U~e37R3kWGVj8,AQACAAE/freesite-HOWTO/4/",
    // More of its bookmarks: the Freenet Privacy and Software Site, and the
    // freesites of FMS (a message board system) and Freemail.
    "freenet:USK@8AEGotJKXJ4ABJy1gKjls4SrrzpshQNoEMAbu0IFA94,blTuIOwf7k9p1R2GCfJSPSaOFzsuXLrFujOnV1Irjag,AQACAAE/fnpss/35/",
    "freenet:USK@0npnMrqZNKRCRoGojZV93UNHCMN-6UU3rRSAmP6jNLE,~BG-edFtdCC1cSH4O3BWdeIYa8Sw5DfyrSV-TKdO5ec,AQACAAE/fms/155/",
    "freenet:USK@cZu5Jx1eTvsN-p0c22uD3y3DvjKwv6bPRIHlCM5~4zQ,IOBbCUJWoNsYYPYcmscE70tFM-QjfWFW~myTPbeGIHg,AQACAAE/index/4/",
    // The Freenet Documentation Wiki, Sone (a social network on Hyphanet)
    // and Publish! (a guide to putting a freesite up).
    "freenet:USK@z6lnOx7PPe16P3Sobu1KM9jiLiIxdvE3L918QMVCLak,AmZ7RemmWcKM2-W4Zs38Ks511lRTsWleMj2mVAReB4E,AQACAAE/freenetdocwiki/17/",
    "freenet:USK@nwa8lHa271k2QvJ8aa0Ov7IHAV-DFOCFgmDt3X6BpCI,DuQSUZiI~agF8c-6tjsFFGuZ8eICrzWCILB60nT8KKo,AQACAAE/sone/83/",
    "freenet:USK@940RYvj1-aowEHGsb5HeMTigq8gnV14pbKNsIvUO~-0,FdTbR3gIz21QNfDtnK~MiWgAf2kfwHe-cpyJXuLHdOE,AQACAAE/publish/3/",
    // From Clean Spider: Bob's Safe Closed Index (an index of freesites that
    // leaves out adult content), the Freenet Information Portal, the Freenet
    // Applications FreeSite, the Frost FAQ and the Unofficial Guide to FMS.
    "freenet:USK@goCPIXupjEc1Nb4yX3iCtxdcKIwqU7XebpxUyn7GudM,VOdTMFgEGQalLFecSIGPJ46qlutbdYIeAC6SSClqHvs,AQACAAE/Bob-Safe-Closed-Index/13/",
    "freenet:USK@D~O-C~so9RSMoJ~FWp9BHmE21npXmg0LzVTh-4xWoXU,bpMTwpHvzDmWzIOHSBkQZrdePdW2QhouvcWnh7KqFA8,AQACAAE/FIP/12/",
    "freenet:USK@ugb~uuscsidMI-Ze8laZe~o3BUIb3S50i25RIwDH99M,9T20t3xoG-dQfMO94LGOl9AxRTkaz~TykFY-voqaTQI,AQACAAE/FAFS/53/",
    "freenet:USK@ZVtoHFm~Lm5FctbjloVYwQ0b5KaAae6TeQGk8fswJs4,kRR5rHBQuNpaiFqZE-v3Rtv0e~LWFFbxbh9tAt44UEM,AQACAAE/FrostFAQinf/49/",
    "freenet:USK@xedmmitRTj9-PXJxoPbD7RY1gf9pKi0OcsRmjNPPIU4,AzFWTYV~9-I~eXis14tIkJ4XkF17gIgZrB294LjFXjc,AQACAAE/fmsguide/6/",
    // Hyphanet's developers and tools: Bombe's flog and jSite, Arne
    // Babenhauserheide's site, the Sharesite plugin, FLIP (IRC over Hyphanet)
    // and dgof (git over Hyphanet).
    "freenet:USK@e3myoFyp5avg6WYN16ImHri6J7Nj8980Fm~aQe4EX1U,QvbWT0ImE0TwLODTl7EoJx2NBnwDxTbLTE6zkB-eGPs,AQACAAE/bombe/136/",
    "freenet:USK@e3myoFyp5avg6WYN16ImHri6J7Nj8980Fm~aQe4EX1U,QvbWT0ImE0TwLODTl7EoJx2NBnwDxTbLTE6zkB-eGPs,AQACAAE/jSite/23/",
    "freenet:USK@LHeuG-SU5ZvYQXTu8Y9mhjCYyQxB-m-W~ryElufr-2M,Ij9l0U1Wa~FQu7GFBf~ciwUbCwpTFv6mWZkS1YuoxQk,AQACAAE/arnebab-org/1314/",
    "freenet:USK@dCnkUL22fAmKbKg-Cftx9j2m4IwyWB0QbGoiq1RSLP8,4d1TDqwRr4tYlsubLrQK~c4h0~FtmE-OXCDmFiI8BB4,AQACAAE/Sharesite/41/",
    "freenet:USK@pGQPA-9PcFiE3A2tCuCjacK165UaX07AQYw98iDQrNA,8gwQ67ytBNR03hNj7JU~ceeew22HVq6G50dcEeMcgks,AQACAAE/flip/17/",
    "freenet:USK@nrDOd1piehaN7z7s~~IYwH-2eK7gcQ9wAtPMxD8xPEs,y61pkcoRy-ccB7BHvLCzt3RUjeMILf8ox26NKvPZ-jk,AQACAAE/dgof/292/",
    // IPFS, by DNSLink name: its documentation, specifications, blog and
    // release downloads, and the Distributed Wikipedia Mirror's snapshots,
    // put on IPFS against censorship (English, Turkish, Russian).
    "ipns://docs.ipfs.tech/",
    "ipns://specs.ipfs.tech/",
    "ipns://blog.ipfs.tech/",
    "ipns://dist.ipfs.tech/",
    "ipns://en.wikipedia-on-ipfs.org/wiki/",
    "ipns://tr.wikipedia-on-ipfs.org/wiki/",
    "ipns://ru.wikipedia-on-ipfs.org/wiki/",
];

/// [`DEFAULT_SEEDS`] as crawl jobs.
pub fn default_seeds() -> Vec<Job> {
    DEFAULT_SEEDS
        .iter()
        .filter_map(|s| Source::parse(s).ok())
        .map(|source| Job { source, depth: 0, site_depth: 0, trust: SEED_TRUST })
        .collect()
}

/// Ahmia's list of onion services reported for abuse, as MD5 digests of
/// their hostnames, from its own onion site.
const AHMIA_BLOCKLIST: &str = "http://juhanurmihxlp77nkq76byazcldy2hlmovfu2epvl5ankdibsot4csyd.onion/blacklist/banned/";
const BLOCKLIST_FILE: &str = "ahmia-banned.txt";
/// A list shorter than this is a broken answer, not a list: keep the one held.
const MIN_BLOCKLIST: usize = 1_000;
const BLOCKLIST_REFRESH: Duration = Duration::from_secs(24 * 60 * 60);
const BLOCKLIST_RETRY: Duration = Duration::from_secs(30 * 60);

/// Puts the last copy of Ahmia's blocklist from `data_dir` into the safety
/// gate. Returns how many sites it holds.
pub fn load_blocklist(safety: &Safety, data_dir: &Path) -> usize {
    match std::fs::read_to_string(data_dir.join(BLOCKLIST_FILE)) {
        Ok(list) => safety.set_published(&list),
        Err(_) => 0,
    }
}

/// With the published blocklist turned off: what was blocked while it was on
/// is due again, once (the held copy of the list goes; turned back on, it is
/// fetched anew), unless the operator's own lists refuse the site. A page
/// with a blocked term is refused again when it is fetched. Returns how many
/// addresses are due again.
pub fn release_blocklist(store: &CrawlStore, safety: &Safety, data_dir: &Path) -> Result<usize> {
    let held = data_dir.join(BLOCKLIST_FILE);
    if !held.exists() {
        return Ok(0);
    }
    let sites = store.blocked_sites()?;
    let released = store.unblock_sites(sites.iter().filter(|site| safety.operator_allows(site)), now_secs())?;
    std::fs::remove_file(&held).with_context(|| format!("removing {}", held.display()))?;
    Ok(released)
}

/// Fetches Ahmia's blocklist over Tor, puts it into the safety gate and
/// keeps a copy in `data_dir`. Returns how many sites it holds.
pub async fn refresh_blocklist(gateway: &Gateway, safety: &Safety, data_dir: &Path) -> Result<usize> {
    let source = Source::parse(AHMIA_BLOCKLIST)?;
    let opts = FetchOptions {
        max_bytes: 16 << 20,
        accept: |content_type| content_type.is_some_and(|t| t.starts_with("text/plain")),
        ..FetchOptions::default()
    };
    let page = gateway.fetch(&source, &opts).await.map_err(|e| anyhow!("{e}"))?;
    anyhow::ensure!(page.status == 200 && !page.truncated, "Ahmia answered {} for its blocklist", page.status);
    let list = String::from_utf8_lossy(page.body.as_deref().unwrap_or_default()).into_owned();
    let digests = list.lines().filter(|l| l.trim().len() == 32).count();
    anyhow::ensure!(digests >= MIN_BLOCKLIST, "Ahmia's blocklist had {digests} entries; keeping the one held");
    let held = safety.set_published(&list);
    std::fs::write(data_dir.join(BLOCKLIST_FILE), &list).context("saving the blocklist")?;
    Ok(held)
}

/// Keeps the blocklist current: refreshed daily, retried half-hourly when
/// Ahmia cannot be reached.
async fn keep_blocklist(gateway: Arc<Gateway>, safety: Arc<Safety>, data_dir: PathBuf, shutdown: CancellationToken) {
    loop {
        let wait = match refresh_blocklist(&gateway, &safety, &data_dir).await {
            Ok(sites) => {
                tracing::info!(sites, "abuse blocklist refreshed from Ahmia");
                BLOCKLIST_REFRESH
            }
            Err(e) => {
                tracing::warn!("abuse blocklist: {e:#}; trying again in 30 minutes");
                BLOCKLIST_RETRY
            }
        };
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = tokio::time::sleep(wait) => {}
        }
    }
}

/// A running daemon: routers, crawler and the local API.
pub struct Daemon {
    addr: SocketAddr,
    socks: Option<SocketAddr>,
    browser_proxy: Option<SocketAddr>,
    gateway: Arc<Gateway>,
    control: Arc<CrawlControl>,
    shutdown: CancellationToken,
    crawl: JoinHandle<StatsSnapshot>,
    server: JoinHandle<std::io::Result<()>>,
}

/// Starts everything and returns once the API is listening. Call inside a
/// multi-threaded Tokio runtime.
pub async fn start(cfg: DaemonConfig) -> Result<Daemon> {
    std::fs::create_dir_all(&cfg.data_dir).with_context(|| format!("creating {}", cfg.data_dir.display()))?;
    // What found on an anonymity network can make this machine connect from
    // its own address: said once, where an operator looks.
    if cfg.gateway.tor_dir.is_some() && !cfg.gateway.clearnet_via_tor {
        tracing::warn!(
            "the crawler reaches the clearnet directly: a page on an onion site can make it fetch an address its \
             author watches, from this machine's IP (--clearnet-via-tor prevents it)"
        );
    }
    if cfg.dht.is_some() {
        tracing::warn!(
            "the BitTorrent DHT sees this machine's IP and every torrent it looks up, those found on onion sites \
             included (--no-dht prevents it)"
        );
    }
    let gateway = Arc::new(Gateway::new(cfg.gateway).map_err(|e| anyhow!("starting the gateway: {e}"))?);
    let index = Arc::new(open_index(&cfg.data_dir)?);
    let store = Arc::new(open_store(&cfg.data_dir)?);
    // The seeds are the operator's choice: a published blocklist does not
    // overrule it, and what one blocked on those sites before is due again.
    let mut safety = cfg.safety;
    let seeds: Vec<&Source> = cfg.seeds.iter().map(|job| &job.source).collect();
    let defaults = default_seeds();
    let chosen: Vec<String> =
        seeds.into_iter().chain(defaults.iter().map(|job| &job.source)).map(Source::site_key).collect();
    safety.choose_sites(&chosen);
    let unblocked = store.unblock_sites(chosen.iter().filter(|site| safety.operator_allows(site)), now_secs())?;
    if unblocked > 0 {
        tracing::info!(unblocked, "addresses on seed sites that a blocklist had refused are due again");
    }
    let safety = Arc::new(safety);
    if cfg.abuse_blocklist {
        let held = load_blocklist(&safety, &cfg.data_dir);
        tracing::info!(sites = held, "abuse blocklist loaded from the last copy");
    } else {
        let released = release_blocklist(&store, &safety, &cfg.data_dir)?;
        if released > 0 {
            tracing::info!(released, "abuse blocklist off: what it blocked is due again");
        }
    }
    tracing::info!(denylist = safety.denylist_len(), blocked_terms = safety.blocked_terms_len(), "safety gate loaded");

    let dht = match cfg.dht {
        Some(dht_cfg) => match Dht::start(dht_cfg).await {
            Ok(dht) => Some(dht),
            Err(e) => {
                tracing::warn!("DHT disabled: {e}");
                None
            }
        },
        None => None,
    };
    let mut crawler = Crawler::new(gateway.clone(), index.clone(), safety.clone(), store.clone(), cfg.crawl);
    if let Some(dht) = &dht {
        crawler = crawler.with_dht(dht.clone());
    }
    let control = crawler.control();
    control.set_paused(cfg.start_paused);
    let (jobs, rx) = mpsc::channel(1024);
    let shutdown = CancellationToken::new();
    let crawl = tokio::spawn(crawler.run(rx, shutdown.clone()));
    if let (Some(dht), true) = (&dht, cfg.dht_harvest) {
        tokio::spawn(harvest(dht.clone(), store.clone(), shutdown.clone()));
    }
    tokio::spawn(watch_routers(gateway.clone(), store, shutdown.clone()));
    if cfg.abuse_blocklist {
        tokio::spawn(keep_blocklist(gateway.clone(), safety.clone(), cfg.data_dir.clone(), shutdown.clone()));
    }
    let seeder = jobs.clone();
    tokio::spawn(async move {
        for job in cfg.seeds {
            if seeder.send(job).await.is_err() {
                break;
            }
        }
    });

    let listener =
        tokio::net::TcpListener::bind(cfg.listen).await.with_context(|| format!("binding {}", cfg.listen))?;
    let addr = listener.local_addr()?;
    if !addr.ip().is_loopback() && cfg.api_token.is_none() {
        tracing::warn!("the API is reachable beyond this machine and has no token; set one");
    }
    let socks = match cfg.socks {
        Some((addr, access)) => {
            let listener =
                tokio::net::TcpListener::bind(addr).await.with_context(|| format!("binding the proxy to {addr}"))?;
            let bound = listener.local_addr()?;
            let private = !matches!(access, ProxyAccess::Open);
            tokio::spawn(socks::serve(listener, gateway.clone(), Arc::new(access), shutdown.clone()));
            tracing::info!(private, "SOCKS5 and HTTP proxy on {bound}");
            Some(bound)
        }
        None => None,
    };
    let browser_proxy = match cfg.browser_proxy {
        Some(BrowserProxy { password, listener }) => {
            let listener = match listener {
                Some(kept) => {
                    kept.set_nonblocking(true)?;
                    tokio::net::TcpListener::from_std(kept)?
                }
                None => tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
                    .await
                    .context("binding the browser's proxy")?,
            };
            let bound = listener.local_addr()?;
            let access = Arc::new(socks::Access::Browser { password });
            tokio::spawn(socks::serve(listener, gateway.clone(), access, shutdown.clone()));
            tracing::info!("browser proxy on {bound}");
            Some(bound)
        }
        None => None,
    };
    let state = api::AppState { gateway: gateway.clone(), index, control: control.clone(), jobs, safety, dht };
    let router = api::router(state, addr, cfg.api_token);
    let stop = shutdown.clone().cancelled_owned();
    let server = tokio::spawn(async move { axum::serve(listener, router).with_graceful_shutdown(stop).await });
    tracing::info!("omnid {} listening on http://{addr}", env!("CARGO_PKG_VERSION"));
    Ok(Daemon { addr, socks, browser_proxy, gateway, control, shutdown, crawl, server })
}

impl Daemon {
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn socks_addr(&self) -> Option<SocketAddr> {
        self.socks
    }

    pub fn browser_proxy_addr(&self) -> Option<SocketAddr> {
        self.browser_proxy
    }

    /// New Tor circuits for every site the browser visits from now on.
    pub fn new_identity(&self) {
        self.gateway.new_identity();
    }

    pub fn control(&self) -> &Arc<CrawlControl> {
        &self.control
    }

    /// The gateway, for what the app asks of the network itself (downloads).
    pub fn gateway(&self) -> &Arc<Gateway> {
        &self.gateway
    }

    /// Stops accepting requests and crawling; [`wait`](Self::wait) returns
    /// once the index is committed.
    pub fn shutdown(&self) {
        self.shutdown.cancel();
    }

    /// For stopping the daemon from another task.
    pub fn shutdown_token(&self) -> CancellationToken {
        self.shutdown.clone()
    }

    pub async fn wait(self) -> Result<StatsSnapshot> {
        self.server.await??;
        // The server stopping for any reason stops the crawl too.
        self.shutdown.cancel();
        Ok(self.crawl.await?)
    }
}

/// How often the daemon looks whether the I2P router, Hyphanet node and IPFS
/// gateway are up.
const ROUTER_CHECK: Duration = Duration::from_secs(30);

/// When the I2P router, the Hyphanet node or the IPFS gateway comes up (the
/// i2pd app started, a node set), what was put off for want of it is crawled
/// now instead of after its deferral.
async fn watch_routers(gateway: Arc<Gateway>, store: Arc<CrawlStore>, shutdown: CancellationToken) {
    let mut was_ready = HashSet::new();
    loop {
        let ready: HashSet<Network> =
            gateway.status().await.into_iter().filter(|r| r.state == RouterState::Ready).map(|r| r.network).collect();
        for network in [Network::I2p, Network::Hyphanet, Network::Ipfs] {
            if ready.contains(&network) && !was_ready.contains(&network) {
                let store = store.clone();
                match tokio::task::spawn_blocking(move || store.retry_deferred(network, now_secs())).await {
                    Ok(Ok(0)) => {}
                    Ok(Ok(n)) => tracing::info!(%network, retried = n, "router up: crawling what waited for it"),
                    Ok(Err(e)) => tracing::warn!(%network, "retrying deferred addresses: {e}"),
                    Err(e) => tracing::warn!(%network, "retrying deferred addresses: {e}"),
                }
            }
        }
        was_ready = ready;
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = tokio::time::sleep(ROUTER_CHECK) => {}
        }
    }
}

/// Writes sampled info hashes to the crawl store, where the crawler picks
/// them up like any other magnet.
async fn harvest(dht: Dht, store: Arc<CrawlStore>, shutdown: CancellationToken) {
    tracing::info!("harvesting info hashes from the DHT");
    let mut hashes = dht.harvest();
    let mut batch = Vec::new();
    loop {
        let received = tokio::select! {
            _ = shutdown.cancelled() => return,
            n = hashes.recv_many(&mut batch, 256) => n,
        };
        if received == 0 {
            return;
        }
        let jobs: Vec<_> = batch
            .drain(..)
            .map(|info_hash| Job {
                source: Source::Magnet(Magnet { info_hash, name: None }),
                depth: 0,
                site_depth: 0,
                trust: HARVESTED_TRUST,
            })
            .collect();
        let store = store.clone();
        match tokio::task::spawn_blocking(move || store.discover(&jobs, now_secs())).await {
            Ok(Ok(added)) => tracing::debug!(added, "harvested"),
            Ok(Err(e)) => tracing::error!("crawl store: {e}"),
            Err(e) => tracing::error!("harvest write died: {e}"),
        }
    }
}

/// The index lives in a directory named for its schema version, so an
/// upgrade builds a new index next to the old one.
pub fn index_dir(data_dir: &Path) -> PathBuf {
    data_dir.join(format!("index-v{SCHEMA_VERSION}"))
}

/// Opens the index, first carrying the previous version's pages over if
/// this version has no index yet.
pub fn open_index(data_dir: &Path) -> Result<SearchIndex> {
    let dir = index_dir(data_dir);
    let previous = data_dir.join(format!("index-v{}", SCHEMA_VERSION - 1));
    if !dir.exists() && previous.exists() {
        migrate_index(&previous, &dir).with_context(|| format!("migrating {}", previous.display()))?;
    }
    SearchIndex::open(&dir).with_context(|| format!("opening {} (is `omnid serve` already running?)", dir.display()))
}

/// Builds the new index aside and renames it into place once committed, so
/// an interrupted upgrade starts over and never leaves half an index.
fn migrate_index(previous: &Path, dir: &Path) -> Result<()> {
    let staging = dir.with_extension("migrating");
    if staging.exists() {
        std::fs::remove_dir_all(&staging)?;
    }
    let copied = SearchIndex::open(&staging)?.migrate_from(previous)?;
    std::fs::rename(&staging, dir)?;
    std::fs::remove_dir_all(previous)?;
    tracing::info!(copied, "carried the index over from {}", previous.display());
    Ok(())
}

pub fn open_store(data_dir: &Path) -> Result<CrawlStore> {
    std::fs::create_dir_all(data_dir).with_context(|| format!("creating {}", data_dir.display()))?;
    let path = data_dir.join("crawl.sqlite");
    CrawlStore::open(&path).with_context(|| format!("opening {}", path.display()))
}

/// One address per line; `#` starts a comment.
pub fn read_seeds(path: &Path) -> Result<Vec<Job>> {
    let list = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    list.lines()
        .enumerate()
        .map(|(n, line)| (n + 1, line.split('#').next().unwrap_or_default().trim()))
        .filter(|(_, line)| !line.is_empty())
        .map(|(n, line)| {
            let source = Source::parse(line).with_context(|| format!("{}:{n}", path.display()))?;
            Ok(Job { source, depth: 0, site_depth: 0, trust: SEED_TRUST })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use omnid_crawl::Verdict;

    #[test]
    fn starts_somewhere_on_every_overlay_network() {
        let seeds = default_seeds();
        assert_eq!(seeds.len(), DEFAULT_SEEDS.len(), "every seed parses");
        for network in [Network::Onion, Network::I2p, Network::Hyphanet, Network::Ipfs] {
            assert!(seeds.iter().any(|j| j.source.network() == network), "{network} has seeds");
        }
        assert!(!seeds.iter().any(|j| j.source.network() == Network::Clearnet), "overlay sites only");
        // Added once: a store that has them gains nothing more.
        let store = CrawlStore::in_memory().unwrap();
        assert_eq!(store.discover(&seeds, 0).unwrap(), seeds.len());
        assert_eq!(store.discover(&default_seeds(), 0).unwrap(), 0);
    }

    /// Onion sites on Ahmia's blocklist of 2026-11-21, as it lists them (the
    /// MD5 of the hostname): five default seeds it names wrongly, then sites
    /// left out of the seeds for being on it.
    const AHMIA_LISTED: &[(&str, &str)] = &[
        ("ProPublica", "047c9505707fbe1f54af4567310288cd"),
        ("Riseup", "b1f4acf9c73f5957510f91758c804325"),
        ("Proton", "902e3abeed429df1548eecaed3df3df3"),
        ("Mullvad", "474f2b685fbec617b47cb5640bc3f7c9"),
        ("Qubes OS", "5497b487cf65f6a0fa26606081a61709"),
        ("Tor Blog", "f5bce0c96660995b7cb3b9d2cf026074"),
        ("Tor Forum", "8ed45ee0976cbd121f427fefcb7141c1"),
        ("Tor mailing lists", "7b114dd5d6c6b3172b51d3489d21ac5b"),
        ("Debian", "ffc346a37a0b0a0fd4948e0873391ae5"),
        ("Planet Debian", "75e8b8b65db55a1bad9c0f6598acd5e3"),
        ("Freedom of the Press Foundation", "3a495bed2fb1e18ec391808c0fe4bab1"),
        ("Free Software Foundation Europe", "fd613fb1cd38ec2801c6d1b0e2404542"),
        ("Disroot", "73d781ae9f6219d8841150658541d30c"),
        ("The Guardian", "3cc3fcdde6a76be8f884fe126bb31397"),
        ("BBC News", "04d6a94372dfc244a3a9957431b9e577"),
        ("Deutsche Welle", "75be80714a7defbdda296b6c06e37fc6"),
        ("taz", "85bc82719b899e64fe8eb08833cf6f14"),
        ("DuckDuckGo", "c7bfbaed0b59d804993973304bdf6d7a"),
        ("Brave", "b1b993f914496d3c49251d1cf5e34ce4"),
        ("SimpleX Chat", "d32b8bccb607727e29d9e03cc0901f01"),
        ("DEF CON", "3318c7d48585d50352486d43a7f46bfa"),
        ("DEF CON Media", "3d9dae3e25aa1019496f004cf1c5aad3"),
        ("CIA", "a609305b13b6d3c156136689961a8de0"),
        ("decoded.legal", "c0fc4d10a8d3162088d5ee794ef11601"),
        ("Cryptostorm", "d2fe6126f48a9838528239a401d56815"),
    ];

    #[test]
    fn adds_no_onion_seed_on_ahmias_blocklist() {
        let safety = Safety::default();
        let list: String = AHMIA_LISTED.iter().map(|(_, digest)| format!("{digest}\n")).collect();
        assert_eq!(safety.set_published(&list), AHMIA_LISTED.len());
        let listed: Vec<String> = default_seeds()
            .iter()
            .filter(|job| safety.check_source(&job.source) == Verdict::DeniedSite)
            .map(|job| job.source.site_key())
            .collect();
        let wrongly = [
            "p53lf57qovyuvwsc6xnrppyply3vtqm7l6pcobkmyqsiofyeznfu5uqd.onion",
            "vww6ybal4bd7szmgncyruucpgfkqahzddi37ktceo3ah7ngmcopnpyyd.onion",
            "protonmailrmez3lotccipshtkleegetolb73fuirgj7r4o4vfu7ozyd.onion",
            "o54hon2e2vj6c7m3aqqu6uyece65by3vgoxxhlqlsvkmacw6a7m7kiad.onion",
            "qubesosfasa4zl44o4tws22di6kepyzfeqv3tg4e3ztknltfxqrymdad.onion",
        ];
        assert_eq!(listed, wrongly, "only the five it names wrongly");
    }

    #[test]
    fn opens_a_store_where_no_data_directory_is_yet() {
        // `omnid serve` asks the store for seeds before the daemon starts:
        // on a first run, nothing has made the directory.
        let parent = std::env::temp_dir().join(format!("omnid-first-run-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&parent);
        let store = open_store(&parent.join("omnid")).unwrap();
        assert_eq!(store.known().unwrap(), 0);
        let _ = std::fs::remove_dir_all(parent);
    }

    #[test]
    fn loads_the_last_copy_of_the_blocklist() {
        let dir = std::env::temp_dir().join(format!("omnid-blocklist-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let safety = Safety::default();
        assert_eq!(load_blocklist(&safety, &dir), 0, "no copy yet");
        let list: String = (0..MIN_BLOCKLIST).map(|i| format!("{i:032x}\n")).collect();
        std::fs::write(dir.join(BLOCKLIST_FILE), &list).unwrap();
        assert_eq!(load_blocklist(&safety, &dir), MIN_BLOCKLIST);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn turning_the_blocklist_off_releases_what_it_blocked_once() {
        let dir = std::env::temp_dir().join(format!("omnid-release-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = CrawlStore::in_memory().unwrap();
        let urls = ["http://listed.example/", "http://denied.example/page"];
        let jobs: Vec<Job> = urls
            .iter()
            .map(|url| Job { source: Source::parse(url).unwrap(), depth: 0, site_depth: 0, trust: SEED_TRUST })
            .collect();
        store.discover(&jobs, 0).unwrap();
        store.take_due(0, 10).unwrap();
        for url in urls {
            store.finish(url, Network::Clearnet, omnid_crawl::Visit::Blocked, 0).unwrap();
        }
        let safety = Safety::from_lists("denied.example", "");
        assert_eq!(release_blocklist(&store, &safety, &dir).unwrap(), 0, "no list was held: nothing it blocked");
        std::fs::write(dir.join(BLOCKLIST_FILE), "0123456789abcdef0123456789abcdef\n").unwrap();
        assert_eq!(release_blocklist(&store, &safety, &dir).unwrap(), 1, "the operator's own denylist still holds");
        assert!(!dir.join(BLOCKLIST_FILE).exists(), "the held copy goes");
        assert_eq!(release_blocklist(&store, &safety, &dir).unwrap(), 0, "once");
        assert_eq!(store.blocked_sites().unwrap(), ["denied.example"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    fn page(url: &str) -> omnid_core::IndexedDoc {
        omnid_core::IndexedDoc {
            url: url.into(),
            network: omnid_core::Network::Clearnet,
            site: "a.example".into(),
            title: "kept across versions".into(),
            description: String::new(),
            keywords: vec![],
            body: String::new(),
            content_hash: url.into(),
            info_hash: None,
            files: vec![],
            first_seen: 1,
            last_seen: 1,
            liveness: 1.0,
            trust: 0.5,
            lang: None,
        }
    }

    #[test]
    fn carries_the_previous_index_over_once() {
        let data = tempfile::tempdir().unwrap();
        let previous = data.path().join(format!("index-v{}", SCHEMA_VERSION - 1));
        let staging = index_dir(data.path()).with_extension("migrating");
        {
            let old = SearchIndex::open(&previous).unwrap();
            old.upsert(&page("http://a.example/")).unwrap();
            old.commit().unwrap();
        }
        // What an upgrade killed halfway leaves behind.
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("meta.json"), "{").unwrap();

        let index = open_index(data.path()).unwrap();
        assert!(index.get("http://a.example/").unwrap().is_some());
        assert!(!previous.exists(), "the old index goes once it is copied");
        assert!(!staging.exists());
        drop(index);
        assert_eq!(open_index(data.path()).unwrap().num_docs(), 1, "the next start opens it as it is");
    }
}
