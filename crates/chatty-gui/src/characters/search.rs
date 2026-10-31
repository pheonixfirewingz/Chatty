//! Fuzzy character search shared by the roster and the new-chat gallery.
//!
//! Matching allocates nothing per character: views call it while building,
//! so it folds case one `char` at a time instead of lowercasing whole fields.

use chatty_protocol::Character;

/// Score weight of a fuzzy name match relative to other fields.
const NAME_WEIGHT: u32 = 3;
/// Score weight of a fuzzy tag match.
const TAG_WEIGHT: u32 = 2;
/// Score of a plain substring hit in a long profile field.
const PROFILE_HIT: u32 = 1;

fn fold(ch: char) -> char {
    ch.to_lowercase().next().unwrap_or(ch)
}

/// Greedily matches `term` as a subsequence of `haystack`. `prev` is the
/// character before `haystack`, used to detect word starts.
fn score_from(term: &str, haystack: &str, mut prev: Option<char>) -> Option<u32> {
    let mut needle = term.chars().map(fold).peekable();
    let (mut score, mut run) = (0, 0);
    for ch in haystack.chars() {
        let Some(&want) = needle.peek() else { break };
        if fold(ch) == want {
            needle.next();
            run += 1;
            let word_start = prev.is_none_or(|p| !p.is_alphanumeric());
            score += 1 + 2 * run + if word_start { 6 } else { 0 };
        } else {
            run = 0;
        }
        prev = Some(ch);
    }
    needle.peek().is_none().then_some(score)
}

/// Scores `term` as a case-insensitive subsequence of `haystack`; higher is
/// better and `None` means some character of `term` is missing. Contiguous
/// runs and word starts score higher, so "ali" ranks "Alice" above "Natalie".
#[must_use]
pub fn fuzzy_score(term: &str, haystack: &str) -> Option<u32> {
    let first = term.chars().next().map(fold)?;
    let mut best = None;
    let mut prev = None;
    for (index, ch) in haystack.char_indices() {
        if fold(ch) == first {
            best = best.max(score_from(term, &haystack[index..], prev));
        }
        prev = Some(ch);
    }
    best
}

/// Case-insensitive substring test for long fields, where a loose
/// subsequence would match almost any short term.
fn contains_folded(haystack: &str, term: &str) -> bool {
    haystack.char_indices().any(|(index, _)| {
        let mut rest = haystack[index..].chars().map(fold);
        term.chars().map(fold).all(|want| rest.next() == Some(want))
    })
}

/// Scores a character against a whitespace-separated query. Every term must
/// match the name or a tag fuzzily, or a profile field as a substring.
#[must_use]
pub fn character_match_score(character: &Character, query: &str) -> Option<u32> {
    let mut total = 0;
    for term in query.split_whitespace() {
        let name = fuzzy_score(term, &character.name).map(|s| s * NAME_WEIGHT);
        let tag = character
            .tags
            .iter()
            .filter_map(|tag| fuzzy_score(term, tag))
            .max()
            .map(|s| s * TAG_WEIGHT);
        let profile = [
            &character.race,
            &character.gender,
            &character.age,
            &character.personality,
            &character.description,
        ]
        .into_iter()
        .any(|field| contains_folded(field, term))
        .then_some(PROFILE_HIT);
        total += name.max(tag).max(profile)?;
    }
    Some(total)
}

/// Characters matching `query`, best match first; ties and a blank query
/// keep the roster order.
#[must_use]
pub fn search_characters<'a>(characters: &'a [Character], query: &str) -> Vec<&'a Character> {
    if query.trim().is_empty() {
        return characters.iter().collect();
    }
    let mut scored: Vec<_> = characters
        .iter()
        .filter_map(|c| character_match_score(c, query).map(|score| (score, c)))
        .collect();
    scored.sort_by_key(|&(score, _)| std::cmp::Reverse(score));
    scored.into_iter().map(|(_, c)| c).collect()
}

/// The portrait shown for a character: its default image, else its first
/// image, else the imported card avatar.
#[must_use]
pub fn portrait_bytes(character: &Character) -> Option<&[u8]> {
    character
        .images
        .iter()
        .find(|img| character.default_image_id.as_deref() == Some(&img.id))
        .or_else(|| character.images.first())
        .map(|img| img.data.as_slice())
        .or(character.avatar.as_deref())
}
