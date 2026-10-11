import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from slugify import slugify


class SlugifyTest(unittest.TestCase):
    def test_basic(self):
        self.assertEqual(slugify("hello world"), "hello-world")

    def test_uppercase(self):
        self.assertEqual(slugify("Hello World"), "hello-world")

    def test_punctuation(self):
        self.assertEqual(slugify("  What's new?! "), "what-s-new")

    def test_max_length_does_not_end_with_hyphen(self):
        self.assertEqual(slugify("alpha beta gamma", max_length=11), "alpha-beta")

    def test_empty(self):
        self.assertEqual(slugify(""), "")


if __name__ == "__main__":
    unittest.main()
