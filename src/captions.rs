//! What the overlay should say, given the transcript so far.

use std::time::Duration;

use crate::moonshine::Line;

/// More than could ever be on screen; older text is cut off a word at a time.
const KEEP: usize = 4000;
/// Quiet for this long after the last word, and the captions clear.
const CLEAR_AFTER: Duration = Duration::from_secs(6);
/// An unchanged guess this old counts as agreeing with itself, so the last words of a pause show. The
/// model decodes about every half second and answers with its last guess between times, so anything
/// shorter would take every guess as agreed.
const HELD_LONG_ENOUGH: Duration = Duration::from_millis(800);
/// A stream keeps every line and its audio, so it's swapped for a new one once it has this many.
const STREAM_LINES: usize = 32;

/// A word without its case and punctuation, for comparing guesses: the model ends nearly every guess
/// as if it were the end of a sentence, so "mind." and "mind" are the same word.
pub fn plain(word: &str) -> String {
    word.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase()
}

/// Steadies the line still being spoken. The model revises its guess as it hears more, so a word only
/// goes up once two guesses in a row agree on it, and what's up stays unless they agree on a change.
#[derive(Default)]
struct Steady {
    line: usize,
    previous: Vec<String>,
    previous_at: Duration,
    shown: Vec<String>,
}

impl Steady {
    fn update(&mut self, line: usize, guess: Vec<String>, now: Duration) -> &[String] {
        if line != self.line {
            *self = Self { line, previous_at: now, ..Self::default() };
        }
        let agreed = if guess == self.previous {
            // The model only decodes every so often, so seeing the same guess again may mean nothing.
            if now.saturating_sub(self.previous_at) >= HELD_LONG_ENOUGH { guess.len() } else { 0 }
        } else {
            let agreed = self.previous.iter().zip(&guess).take_while(|(a, b)| plain(a) == plain(b)).count();
            self.previous_at = now;
            agreed
        };
        if agreed > 0 && agreed >= self.shown.len() {
            self.shown = guess[..agreed].to_vec();
            // Whether this is really the end of a sentence isn't known until the line's finished.
            if let Some(last) = self.shown.last_mut() {
                *last = last.trim_end_matches(|c: char| !c.is_alphanumeric()).to_owned();
            }
        }
        self.previous = guess;
        &self.shown
    }
}

#[derive(Default)]
pub struct Captions {
    /// Text carried over from streams since replaced.
    carried: String,
    /// Lines of the current stream that have been cleared off the screen.
    skipped: usize,
    shown: String,
    changed_at: Duration,
    /// Whether the transcript ended in a line still being spoken, last we looked.
    speaking: bool,
    /// Writes without spaces, so a guess is steadied by the character.
    unspaced: bool,
    steady: Steady,
}

impl Captions {
    pub fn new(language: &str) -> Self {
        Self { unspaced: matches!(language, "ja" | "zh"), ..Self::default() }
    }

    /// Takes the transcript at time `now` and returns the text to show, if that's different.
    pub fn update(&mut self, lines: &[Line], now: Duration) -> Option<String> {
        self.speaking = lines.last().is_some_and(|line| !line.complete);
        let steadied = self.steadied(lines, now);
        let text = self.compose(&steadied);
        if text != self.shown {
            self.shown = text.clone();
            self.changed_at = now;
            return Some(text);
        }
        if !self.shown.is_empty() && !self.speaking && now.saturating_sub(self.changed_at) >= CLEAR_AFTER {
            self.carried.clear();
            self.skipped = steadied.len();
            self.shown.clear();
            return Some(String::new());
        }
        None
    }

    /// The lines with the one still being spoken steadied.
    fn steadied(&mut self, lines: &[Line], now: Duration) -> Vec<Line> {
        let mut lines = lines.to_vec();
        let last = lines.len().wrapping_sub(1);
        if let Some(line) = lines.last_mut().filter(|line| !line.complete) {
            let guess: Vec<String> = if self.unspaced {
                line.text.chars().map(String::from).collect()
            } else {
                line.text.split_whitespace().map(str::to_owned).collect()
            };
            let separator = if self.unspaced { "" } else { " " };
            line.text = self.steady.update(last, guess, now).join(separator);
        }
        lines
    }

    /// Whether the stream should be swapped now: long enough, and between sentences.
    pub fn wants_new_stream(&self, lines: &[Line]) -> bool {
        lines.len() >= STREAM_LINES && !self.speaking
    }

    /// Notes that `lines` are the last of a stream being swapped for a new one.
    pub fn stream_replaced(&mut self, lines: &[Line]) {
        self.carried = self.compose(lines);
        self.skipped = 0;
        self.steady = Steady::default();
    }

    fn compose(&self, lines: &[Line]) -> String {
        let separator = if self.unspaced { "" } else { " " };
        let parts = std::iter::once(self.carried.as_str())
            .chain(lines.iter().skip(self.skipped).map(|line| line.text.as_str()))
            .filter(|part| !part.is_empty());
        let text = parts.collect::<Vec<_>>().join(separator);
        tail(&text, KEEP).to_owned()
    }
}

/// The end of `text`, at most `keep` bytes, starting on a word when there are spaces to start on.
fn tail(text: &str, keep: usize) -> &str {
    if text.len() <= keep {
        return text;
    }
    let mut start = text.len() - keep;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    let rest = &text[start..];
    match rest.find(' ') {
        Some(space) => &rest[space + 1..],
        None => rest,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str, complete: bool) -> Line {
        Line { text: text.to_owned(), complete }
    }

    fn at(seconds: u64) -> Duration {
        Duration::from_secs(seconds)
    }

    fn ms(millis: u64) -> Duration {
        Duration::from_millis(millis)
    }

    /// What's up after this guess at the line being spoken, 200ms after the last.
    fn guesses(guesses: &[&str]) -> Vec<String> {
        let mut captions = Captions::new("en");
        let mut shown = Vec::new();
        for (i, guess) in guesses.iter().enumerate() {
            if let Some(text) = captions.update(&[line(guess, false)], ms(i as u64 * 200)) {
                shown.push(text);
            }
        }
        shown
    }

    #[test]
    fn a_word_goes_up_once_two_guesses_agree() {
        assert_eq!(guesses(&["the", "the cat", "the cat sat"]), ["the", "the cat"]);
    }

    #[test]
    fn a_passing_wrong_guess_never_shows() {
        // "cap" is only ever the end of a guess, so it never goes up.
        assert_eq!(guesses(&["the", "the cap", "the cat", "the cat sat"]), ["the", "the cat"]);
    }

    #[test]
    fn what_is_up_stays_until_guesses_agree_on_a_change() {
        // "the cap" goes up, then a single different guess doesn't take it back.
        let mut captions = Captions::new("en");
        captions.update(&[line("the cap", false)], ms(0));
        captions.update(&[line("the cap on", false)], ms(200));
        assert_eq!(captions.shown, "the cap");
        assert_eq!(captions.update(&[line("a cat", false)], ms(400)), None);
        assert_eq!(captions.update(&[line("a cat on", false)], ms(600)).as_deref(), Some("a cat"));
    }

    #[test]
    fn the_last_words_show_when_the_guess_holds() {
        let mut captions = Captions::new("en");
        assert_eq!(captions.update(&[line("the end", false)], ms(0)), None);
        assert_eq!(captions.update(&[line("the end", false)], ms(100)), None);
        assert_eq!(captions.update(&[line("the end", false)], ms(500)), None);
        assert_eq!(captions.update(&[line("the end", false)], ms(800)).as_deref(), Some("the end"));
    }

    #[test]
    fn a_guess_ending_like_a_sentence_doesnt_count_as_a_different_word() {
        // Each guess has a false ending, which never shows; the words are all that goes up.
        assert_eq!(
            guesses(&["The.", "The model.", "The model keeps.", "The model keeps changing."]),
            ["The", "The model", "The model keeps"]
        );
    }

    #[test]
    fn comparing_ignores_case_and_punctuation_only() {
        assert_eq!(plain("Mind,"), "mind");
        assert_eq!(plain("don't."), "don't");
        assert_ne!(plain("cat"), plain("cap"));
    }

    #[test]
    fn repeating_a_guess_between_decodes_isnt_agreement() {
        let mut captions = Captions::new("en");
        assert_eq!(captions.update(&[line("the wrong", false)], ms(0)), None);
        assert_eq!(captions.update(&[line("the wrong", false)], ms(100)), None);
        assert_eq!(captions.update(&[line("the wrong", false)], ms(400)), None);
        assert_eq!(captions.update(&[line("the right", false)], ms(500)).as_deref(), Some("the"));
    }

    #[test]
    fn a_finished_line_shows_as_the_model_ended_it() {
        let mut captions = Captions::new("en");
        captions.update(&[line("the cap", false)], ms(0));
        assert_eq!(captions.update(&[line("The cat.", true)], ms(200)).as_deref(), Some("The cat."));
    }

    #[test]
    fn it_joins_lines_and_reports_only_changes() {
        let mut captions = Captions::new("en");
        let lines = [line("Hello there.", true), line("How are", true)];
        assert_eq!(captions.update(&lines, at(0)).as_deref(), Some("Hello there. How are"));
        assert_eq!(captions.update(&lines, at(1)), None);
    }

    #[test]
    fn empty_lines_add_no_gaps() {
        let mut captions = Captions::new("en");
        assert_eq!(captions.update(&[line("", true), line("Hi", true), line("", true)], at(0)).as_deref(), Some("Hi"));
    }

    #[test]
    fn japanese_has_no_spaces_between_lines() {
        let mut captions = Captions::new("ja");
        assert_eq!(captions.update(&[line("はい。", true), line("そう", true)], at(0)).as_deref(), Some("はい。そう"));
    }

    #[test]
    fn it_clears_after_a_silence_and_starts_clean() {
        let mut captions = Captions::new("en");
        let mut lines = vec![line("Goodbye.", true)];
        captions.update(&lines, at(0));
        assert_eq!(captions.update(&lines, at(5)), None);
        assert_eq!(captions.update(&lines, at(6)).as_deref(), Some(""));
        assert_eq!(captions.update(&lines, at(20)), None);
        lines.push(line("Back", true));
        assert_eq!(captions.update(&lines, at(21)).as_deref(), Some("Back"));
    }

    #[test]
    fn it_never_clears_someone_still_talking() {
        let mut captions = Captions::new("en");
        let lines = [line("A very long pause", false)];
        captions.update(&lines, at(0));
        assert_eq!(captions.update(&lines, at(60)).as_deref(), Some("A very long pause"));
        assert_eq!(captions.update(&lines, at(120)), None);
    }

    #[test]
    fn long_text_is_cut_on_a_word() {
        let text = "word ".repeat(200);
        let mut captions = Captions::new("en");
        let shown = captions.update(&[line(text.trim(), true)], at(0)).unwrap();
        assert!(shown.len() <= KEEP && shown.starts_with("word") && shown.ends_with("word"));
    }

    #[test]
    fn cutting_respects_characters_without_spaces() {
        assert!(tail(&"あ".repeat(400), KEEP).chars().all(|c| c == 'あ'));
    }

    #[test]
    fn swapping_streams_keeps_the_text_on_screen() {
        let mut captions = Captions::new("en");
        let lines = vec![line("One.", true); STREAM_LINES];
        captions.update(&lines, at(0));
        assert!(captions.wants_new_stream(&lines));
        let before = captions.shown.clone();
        captions.stream_replaced(&lines);
        assert_eq!(captions.update(&[], at(1)), None);
        assert_eq!(captions.shown, before);
        assert_eq!(captions.update(&[line("Two", true)], at(2)), Some(format!("{before} Two")));
    }

    #[test]
    fn a_stream_isnt_swapped_mid_sentence() {
        let mut captions = Captions::new("en");
        let mut lines = vec![line("One.", true); STREAM_LINES];
        lines.push(line("Two", false));
        captions.update(&lines, at(0));
        assert!(!captions.wants_new_stream(&lines));
    }
}
