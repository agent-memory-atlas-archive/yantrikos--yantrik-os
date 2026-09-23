//! A folder's two numbers, read off what the mail server actually said.
//!
//! Every folder this service listed said `unread: 0`, on every machine, from the day it was
//! written. `imap_list_folders` sent `STATUS name (UNSEEN)` and read `.unseen` off the answer, and
//! the answer never carries it: the `imap` crate hands every `* STATUS` line to
//! `Session::unsolicited_responses`, and the `Mailbox` that `status()` returns is built from the
//! other lines, which for a STATUS hold nothing. `.unwrap_or(0)` turned "not told" into "none",
//! and the Email app printed it under a header that counted nine unread in the same folder
//! (#74, #123). Had `.unseen` ever been set it would still have been the wrong number: on a
//! `Mailbox` it is the sequence number of the first unseen message, from EXAMINE's `[UNSEEN n]`
//! code, not how many there are.
//!
//! The other number in that header, "of 21" for a folder of 35, was this service's too: a page
//! of twenty was asked for and twenty-one came back, because both ends of the FETCH range were
//! inclusive. [`page_range`] is that arithmetic, done once.
//!
//! Nothing here touches a socket, so the tests at the bottom can say what a server said and
//! check what is made of it.

use imap::types::{StatusAttribute, UnsolicitedResponse};

/// What is asked of each folder. Both numbers in one round trip, and without an EXAMINE first:
/// selecting a folder just to read its size was a second command per folder, and RFC 3501 says
/// STATUS is for a mailbox that is *not* selected.
pub const STATUS_ITEMS: &str = "(MESSAGES UNSEEN)";

/// A folder's size and how much of it is unread, as the server counts them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    pub total: u32,
    pub unread: u32,
}

/// The counts for `folder` among the STATUS responses the server has sent since they were last
/// read — or `None` when it sent none for that folder. A `\Noselect` container like Gmail's
/// `[Gmail]` answers STATUS with a refusal, and a refusal is not zero.
///
/// The name is matched as the server echoed it, which is the name LIST gave, because that is
/// what was sent. If the server sent more than one for the folder, the last one is the newest.
pub fn counts_for(
    folder: &str,
    responses: impl IntoIterator<Item = UnsolicitedResponse>,
) -> Option<Counts> {
    let mut found = None;
    for response in responses {
        let UnsolicitedResponse::Status { mailbox, attributes } = response else { continue };
        if mailbox != folder {
            continue;
        }
        let mut counts = Counts::default();
        for attribute in attributes {
            match attribute {
                StatusAttribute::Messages(n) => counts.total = n,
                StatusAttribute::Unseen(n) => counts.unread = n,
                _ => {}
            }
        }
        found = Some(counts);
    }
    found
}

/// The sequence numbers `page` of a folder holding `total` messages covers, newest page first,
/// as the inclusive `start:end` an IMAP FETCH takes — or `None` when the page is past the end.
///
/// The range used to be `total - page * per_page` to `total - (page - 1) * per_page`, both ends
/// inclusive: a page of twenty was twenty-one messages, and the oldest message of one page was
/// the newest of the next. The Email app's header said "9 unread of 21" for a folder of 35
/// because of it.
pub fn page_range(total: u32, page: u32, per_page: u32) -> Option<(u32, u32)> {
    let page = page.max(1);
    let per_page = per_page.max(1);
    let end = total.checked_sub((page - 1).saturating_mul(per_page))?;
    if end == 0 {
        return None;
    }
    let start = end.saturating_sub(per_page) + 1;
    Some((start, end))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(mailbox: &str, attributes: Vec<StatusAttribute>) -> UnsolicitedResponse {
        UnsolicitedResponse::Status { mailbox: mailbox.to_string(), attributes }
    }

    #[test]
    fn the_counts_are_read_off_the_status_response_for_that_folder() {
        let counts = counts_for(
            "INBOX",
            vec![
                UnsolicitedResponse::Exists(35),
                status("INBOX", vec![StatusAttribute::Messages(35), StatusAttribute::Unseen(12)]),
            ],
        );
        assert_eq!(counts, Some(Counts { total: 35, unread: 12 }));
    }

    #[test]
    fn another_folders_status_is_not_this_folders() {
        let counts = counts_for(
            "[Gmail]/Spam",
            vec![status("INBOX", vec![StatusAttribute::Messages(35), StatusAttribute::Unseen(12)])],
        );
        assert_eq!(counts, None);
    }

    #[test]
    fn a_folder_the_server_would_not_report_is_not_told_as_empty() {
        // `[Gmail]` is \Noselect; STATUS on it is refused and nothing arrives. That is "not
        // known", which is what the caller has to be told, not a pair of zeros.
        assert_eq!(counts_for("[Gmail]", Vec::<UnsolicitedResponse>::new()), None);
    }

    #[test]
    fn a_status_with_no_unseen_item_is_zero_unread_and_not_missing() {
        // The server was asked for both and answered with one. MESSAGES without UNSEEN is a
        // folder with nothing unseen only if the server says so; here it did not, so this is
        // the least wrong reading rather than a guarantee. The point of the test is that the
        // total survives.
        let counts =
            counts_for("Archive", vec![status("Archive", vec![StatusAttribute::Messages(40)])]);
        assert_eq!(counts, Some(Counts { total: 40, unread: 0 }));
    }

    #[test]
    fn the_last_status_for_a_folder_is_the_one_that_counts() {
        let counts = counts_for(
            "INBOX",
            vec![
                status("INBOX", vec![StatusAttribute::Messages(35), StatusAttribute::Unseen(12)]),
                status("INBOX", vec![StatusAttribute::Messages(36), StatusAttribute::Unseen(13)]),
            ],
        );
        assert_eq!(counts, Some(Counts { total: 36, unread: 13 }));
    }

    #[test]
    fn a_page_of_twenty_is_twenty_messages() {
        // The folder on the machine this was found on: 35 messages, page 1 of 20.
        let (start, end) = page_range(35, 1, 20).unwrap();
        assert_eq!((start, end), (16, 35));
        assert_eq!(end - start + 1, 20);
    }

    #[test]
    fn consecutive_pages_do_not_share_a_message() {
        let first = page_range(35, 1, 20).unwrap();
        let second = page_range(35, 2, 20).unwrap();
        assert_eq!(second, (1, 15));
        assert_eq!(first.0, second.1 + 1);
        assert_eq!(page_range(35, 3, 20), None);
    }

    #[test]
    fn a_folder_smaller_than_a_page_is_one_page() {
        assert_eq!(page_range(5, 1, 20), Some((1, 5)));
        assert_eq!(page_range(5, 2, 20), None);
    }

    #[test]
    fn an_empty_folder_has_no_page() {
        assert_eq!(page_range(0, 1, 20), None);
    }

    #[test]
    fn a_page_that_ends_exactly_on_the_first_message_is_whole() {
        assert_eq!(page_range(40, 2, 20), Some((1, 20)));
        assert_eq!(page_range(40, 3, 20), None);
    }
}
