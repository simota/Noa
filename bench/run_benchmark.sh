#!/bin/bash

# Ensure data files are generated
if [ ! -f "150MB_ascii.txt" ] || [ ! -f "150MB_unicode.txt" ]; then
  echo "Benchmark files not found. Generating data files (150MB each)..."
  python3 generate_data.py
fi

echo "========================================="
echo "Terminal IO Throughput Benchmarks"
echo "========================================="
echo "Note: To measure terminal rendering speed, the output must be displayed on screen."
echo "This will flood your terminal with text for a moment."
echo ""
echo "Ready to run: time cat 150MB_ascii.txt"
read -p "Press [Enter] to start ASCII benchmark..."

echo "--- START: ASCII BENCHMARK ---"
# Render to screen (stdout), but capture time output (stderr)
{ time cat 150MB_ascii.txt; } 2> .ascii_time.txt
echo "--- END: ASCII BENCHMARK ---"
echo ""

echo "Ready to run: time cat 150MB_unicode.txt"
read -p "Press [Enter] to start Unicode benchmark..."

echo "--- START: UNICODE BENCHMARK ---"
# Render to screen (stdout), but capture time output (stderr)
{ time cat 150MB_unicode.txt; } 2> .unicode_time.txt
echo "--- END: UNICODE BENCHMARK ---"
echo ""

# Parse elapsed times
ascii_real=$(grep real .ascii_time.txt | awk '{print $2}')
unicode_real=$(grep real .unicode_time.txt | awk '{print $2}')

# Clean up temp files
rm -f .ascii_time.txt .unicode_time.txt

echo "========================================="
echo "           BENCHMARK SUMMARY"
echo "========================================="
echo "Your Terminal Performance:"
echo "  - ASCII (150MB_ascii.txt):      $ascii_real"
echo "  - Unicode (150MB_unicode.txt):  $unicode_real"
echo "========================================="
echo "Benchmarks completed!"
