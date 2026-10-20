import argparse
import os

parser = argparse.ArgumentParser()
parser.add_argument("--lang", default="en")
parser.add_argument("--pages", type=int, default=0)
args = parser.parse_args()

print(f"env:  lang={os.environ.get('lang', '-')} pages={os.environ.get('pages', '-')}")
print(f"argv: lang={args.lang} pages={args.pages}")
