# Browser eval subject

A minimal browser-only agent with one `browser` inference slot.

## Configuration

Bind the `browser` inference slot to the profile under evaluation. The eval harness
must install gents/web with its service authority and supply a unique browser
session in each fresh trial. The native worker connection is injected by Gents. This pack alone does not install or start Chromium.

## Usage

Use this directory as the cell subject for `browser-use-core`. See the parent
eval README for the integration contract and current execution limitations.
