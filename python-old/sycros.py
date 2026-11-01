#!/usr/bin/env python3
"""Run sycrOS without installing:  python3 sycros.py --help"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from sycros.cli import main  # noqa: E402

main()
