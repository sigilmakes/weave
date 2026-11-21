#!/usr/bin/env python3
import unittest

from instrument_test_timings import instrument


class InstrumentTests(unittest.TestCase):
    def test_keeps_attributes_and_test_bodies(self):
        source = '''#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[should_panic]
async fn sample() { panic!("expected"); }
'''
        expected = source.replace("#[should_panic]", "#[pa_test_scratch::pa_test]\n#[should_panic]")
        self.assertEqual(instrument(source), (expected, 1))
        self.assertEqual(instrument(expected), (expected, 0))

    def test_ignores_strings_and_nested_comments(self):
        source = '''let sample = r###"
#[test]
fn quoted() {}
"###;
/* outer /* inner */
#[test]
fn commented() {}
*/
#[test]
fn actual() { let text = "\\\"escaped"; }
'''
        expected = source.replace("#[test]\nfn actual", "#[pa_test_scratch::pa_test]\n#[test]\nfn actual")
        self.assertEqual(instrument(source), (expected, 1))


if __name__ == "__main__":
    unittest.main()
