#!/bin/bash

# Script to fetch a list of URLs and extract the 'href' value from 
# any <link> tag that has the attribute rel="alternate" using htmlq.

# --- Configuration ---
INPUT_FILE=${1}
OUTPUT_FILE=${2}

PATH="$HOME/.cargo/bin:$PATH"

# Check for required tools
if ! command -v htmlq &> /dev/null; then
    echo "Error: 'htmlq' command not found." >&2
    echo "Please install htmlq (a command-line HTML parser)." >&2
    exit 1
fi

# Check for required arguments
if [ -z "$INPUT_FILE" ] || [ -z "$OUTPUT_FILE" ]; then
    echo "Usage: $0 <input_file_with_urls> <output_file_name>" >&2
    echo "Example: $0 input_urls.txt alternate_links.txt" >&2
    exit 1
fi

# Clear the output file if it exists, or create it
> "$OUTPUT_FILE"
echo "Processing URLs from $INPUT_FILE. Results will be saved to $OUTPUT_FILE." >&2
echo "----------------------------------------------------------------------" >&2

# Check if the input file exists
if [ ! -f "$INPUT_FILE" ]; then
    echo "Error: Input file '$INPUT_FILE' not found." >&2
    exit 1
fi

# Read the input file line by line
while IFS= read -r url; do
    # Skip empty lines
    if [ -z "$url" ]; then
        continue
    fi
    
    # 1. Fetch the page content silently (-s) and follow redirects (-L)
    # 2. Use htmlq to select the elements and extract the attribute value.
    #    Selector: 'link[rel="alternate"]'
    #    Attribute: '--attr href'
    
    # Capture all alternate URLs found on the page (can be multiple lines)
    alternate_urls=$(
        curl -sL "$url" 2>/dev/null | 
        htmlq 'link[rel="canonical"]' --attribute href
    )

    if [ -n "$alternate_urls" ]; then
        # Use a here-string to read the multi-line output from htmlq
        while IFS= read -r found_url; do
            if [ -n "$found_url" ]; then
                # Append the found URL to the output file (standard data destination)
                echo "$found_url" >> "$OUTPUT_FILE"
            fi
        done <<< "$alternate_urls"
    else
        echo "Processing URL: $url" >&2
        echo "   -> Not found or failed to parse." >&2
    fi

done < "$INPUT_FILE"

echo "----------------------------------------------------------------------" >&2
echo "Processing complete. Found alternate URLs saved to $OUTPUT_FILE." >&2