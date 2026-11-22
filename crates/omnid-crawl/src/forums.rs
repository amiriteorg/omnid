//! Forum pages that are never worth a fetch: logging in and signing up,
//! forms (reply, new topic, report, private messages), searches, member
//! lists and profiles, and print views. A forum's robots.txt often leaves
//! them open, and they take a site's page budget from its threads: crawling
//! 80 addresses each, 45 of Discourse Meta's were user pages, 22 of NodeBB's
//! and 19 of Lemmy's.
//!
//! By address only, as the crawler finds links: Discourse, NodeBB, Lemmy,
//! Flarum and XenForo by path segment; phpBB, MyBB, vBulletin and FluxBB by
//! script; SMF by its `action=`. The words are rare enough elsewhere that a
//! site that is no forum loses little.

use url::Url;

/// Path segments, anywhere in the path, that are a forum's sign-in, people
/// or search pages.
const SEGMENTS: [&str; 26] = [
    // Signing in and up.
    "login",
    "signin",
    "sign-in",
    "logout",
    "signup",
    "sign-up",
    "register",
    "lost-password",
    "forgot-password",
    "password-reset",
    "reset-password",
    // People: Discourse, Lemmy and Flarum (/u/), NodeBB (/user/, /users),
    // XenForo (/members/).
    "u",
    "user",
    "users",
    "member",
    "members",
    "profile",
    // Forms and feeds of the moment.
    "search",
    "create_post",
    "create_community",
    "new-topic",
    "newtopic",
    "compose",
    "conversations",
    "whats-new",
    "notifications",
];

/// Path segments that are a form or a print view only after a post's or
/// topic's number: XenForo's `/posts/123/reply`, Discourse's
/// `/t/slug/123/print`. Elsewhere they can be a page's own name.
const AFTER_AN_ID: [&str; 4] = ["reply", "report", "print", "quote"];

/// Scripts of phpBB, MyBB, vBulletin and FluxBB that are forms, searches or
/// people.
const SCRIPTS: [&str; 20] = [
    "ucp.php",
    "mcp.php",
    "posting.php",
    "memberlist.php",
    "search.php",
    "viewonline.php",
    "login.php",
    "register.php",
    "member.php",
    "usercp.php",
    "private.php",
    "newreply.php",
    "newthread.php",
    "editpost.php",
    "sendthread.php",
    "printthread.php",
    "reputation.php",
    "online.php",
    "showteam.php",
    "userlist.php",
];

/// SMF's and ElkArte's `index.php?action=` values that are the same.
const ACTIONS: [&str; 18] = [
    "login",
    "login2",
    "logout",
    "register",
    "signup",
    "reminder",
    "profile",
    "post",
    "post2",
    "search",
    "search2",
    "printpage",
    "reporttm",
    "unread",
    "unreadreplies",
    "who",
    "mlist",
    "pm",
];

/// Whether `url` is one of the forum pages that are not worth a fetch.
pub fn is_chaff(url: &Url) -> bool {
    let path = url.path().to_ascii_lowercase();
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if segments.iter().any(|segment| SEGMENTS.contains(segment)) {
        return true;
    }
    let after_an_id =
        segments.windows(2).any(|w| AFTER_AN_ID.contains(&w[1]) && w[0].bytes().any(|b| b.is_ascii_digit()));
    if after_an_id {
        return true;
    }
    match segments.last() {
        Some(script) if SCRIPTS.contains(script) => true,
        Some(&"index.php") | None => url.query_pairs().any(|(key, value)| {
            key == "action"
                && ACTIONS.contains(&value.split(';').next().unwrap_or_default().to_ascii_lowercase().as_str())
        }),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chaff(url: &str) -> bool {
        is_chaff(&Url::parse(url).unwrap())
    }

    #[test]
    fn skips_forms_people_and_searches() {
        for url in [
            // Discourse, NodeBB, Lemmy, XenForo, as the crawl found them.
            "https://meta.discourse.org/u/sam",
            "https://meta.discourse.org/u/sam/activity",
            "https://meta.discourse.org/login",
            "https://forum.example/t/some-topic/215462/print",
            "https://community.nodebb.org/user/julian",
            "https://community.nodebb.org/users",
            "https://lemmy.ml/signup",
            "https://lemmy.ml/create_post",
            "https://lemmy.ml/search?q=x",
            "https://xenforo.com/community/members/beerforo.193135/",
            "https://xenforo.com/community/search/?type=post",
            "https://xenforo.com/community/whats-new/posts/",
            "https://xenforo.com/community/posts/1234/reply",
            // phpBB, MyBB, vBulletin.
            "http://forum.example/ucp.php?mode=register&sid=0123",
            "http://forum.example/memberlist.php?mode=viewprofile&u=2",
            "http://forum.example/posting.php?mode=reply&t=5",
            "http://forum.example/forum/member.php?action=profile&uid=1",
            "http://forum.example/newreply.php?tid=9",
            // SMF.
            "http://forum.example/index.php?action=profile;u=1",
            "http://forum.example/index.php?action=login",
            "http://forum.example/?action=printpage;topic=5.0",
        ] {
            assert!(chaff(url), "{url}");
        }
    }

    #[test]
    fn keeps_threads_boards_and_ordinary_pages() {
        for url in [
            "https://meta.discourse.org/t/login-only-does-it-stop-traffic/215462",
            "https://meta.discourse.org/c/support/6",
            "https://community.nodebb.org/topic/18883/how-push-notification-works",
            "https://lemmy.ml/post/55623564",
            "https://lemmy.ml/c/privacy",
            "https://xenforo.com/community/threads/xenforo-2-3-is-here.224075/",
            "https://xenforo.com/community/forums/announcements.1/",
            "http://forum.example/viewtopic.php?t=123",
            "http://forum.example/viewforum.php?f=2",
            "http://forum.example/showthread.php?tid=9",
            "http://forum.example/index.php?topic=5.0",
            "http://forum.example/index.php?action=recent",
            "https://example.com/reports/annual-report",
            "https://example.com/report",
            "https://example.com/print/",
            "https://example.com/blog/users-guide",
        ] {
            assert!(!chaff(url), "{url}");
        }
    }
}
