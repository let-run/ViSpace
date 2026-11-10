# ViSpace Supplier Purchase

To download a build, run **Actions → Build ViSpace Chrome extension** in
GitHub and download the **ViSpace-Chrome-extension** artifact from the completed
run. Extract the extension ZIP and use **Load unpacked** in Chrome to select the
folder containing `manifest.json`.

1. Rebuild and run ViSpace with this checkout with the Chrome extension listener.
2. Start the app's Chrome extension listener (default `127.0.0.1:8824`).
3. Open Chrome's Extensions page (`chrome://extensions`), enable **Developer mode**, choose **Load unpacked**, and select this `chrome-extension` folder.
4. Open the extension, expand **Destinations**, enter the desktop WebSocket address including the port (for example `wss://127.0.0.1:8824`), then choose **Connect & load projects**. Allow access to that host when prompted.
5. Open an LCSC, DigiKey, or Mouser product detail page and click **Read current product**. Review or correct the extracted fields, image URL, and datasheet URL.
6. Enter the purchase quantity, check the desired projects, and enter the quantity allocated to each. Their sum must not exceed the purchase quantity. Unallocated purchases are allowed.
7. Click **Add to Purchase**, allow access to the image host, and keep the popup open until success appears.

The extension reuses the application's icons. It reads only the active page when invoked and requests API/image host access as needed. Settings are stored locally in Chrome. Photos are downloaded, resized to JPEG (up to 800 pixels), and uploaded with the purchase; datasheets remain links. Image errors stop the import so they are not silently omitted. Clear the image URL explicitly to import without a photo.

LCSC layouts vary; the scraper uses Product JSON-LD, metadata, and visible specification labels. Missing fields remain editable. Review the datasheet link and part number before saving. Closing the popup interrupts work; after a connection error during submission, check the app before retrying to avoid duplicates. The app must remain running and reachable. The desktop endpoint uses WSS and accepts only loopback requests with a Chrome-extension Origin. Trust the local test CA in Chrome/OS before connecting. It is separate from mesh membership authentication.

Implementation follows Chrome's [activeTab](https://developer.chrome.com/docs/extensions/develop/concepts/activeTab) and [optional host permissions](https://developer.chrome.com/docs/extensions/develop/concepts/declare-permissions) model.

The editable **Supplier unit price** field is saved as the part custom field **LCSC Price**, **DigiKey Price**, or **Mouser Price**.
The extension selects the unit price for the purchase quantity from visible
quantity tiers and updates it when quantity changes. If no tier matches, it falls
back to the one-item tier or available scalar unit price. If neither is available,
the price stays blank. Manually editing the price overrides automatic selection.
The custom field stores only the selected unit price, including currency.
Missing prices remain blank. Review the currency and quantity tier before adding;
this is a snapshot of displayed pricing and does not calculate a checkout total.
Run scraper checks with `node chrome-extension/tests/price.test.cjs`.


Imports send explicit `part.supplier` (`LCSC`, `DigiKey`, or `Mouser`) and `part.spn`;
manufacturer part number remains in `part.number`. The legacy `lcsc` API field
is no longer sent. Review the editable supplier and supplier part number fields.
DigiKey extraction supports English product detail pages on regional DigiKey
sites. It prefers cut-tape SKU and pricing when listed, keeps each pricing table
separate, and retains photos, datasheets, and project allocations. Verify the
packaging before saving; localized labels may require manual correction.

DigiKey layout reference: [example product page](https://www.digikey.ie/en/products/detail/texas-instruments/TPS63070RNMR/6175216).
Run DigiKey fixture checks with `node chrome-extension/tests/digikey.test.cjs`.

### Browser and desktop destinations

Reload the unpacked extension after updating it, then open **Destinations**.
Choose **Browser app** to send directly to an open ViSpace tab, or **Desktop ·
WebSocket** to use the desktop listener. Click **Connect & load projects** and
grant access to the selected app. The allocation picker loads projects from
that app, and the purchase goes there.

The optional browser URL helps find apps hosted at another address. If a save
fails, the popup keeps the purchase and its request ID so you can retry against
the original app. Read another product to start a new purchase or change the
destination after sending.

The browser bridge accepts only health, project-list, and purchase-import messages,
checks same-window/origin messages, and confirms purchases after IndexedDB finishes
saving. It never starts a web HTTP listener. The extension requests tab metadata to
find open app tabs and requests host access only when connecting to the selected app.

### Discover nearby desktop nodes

Build the native helper with `cargo build --bin vispace-discovery` from the
repository root. Then, from the workspace root, register it for this extension:

```sh
python3 script/install_chrome_discovery.py <extension-id>
```

Find the ID on Chrome's Extensions page. Use `--helper` to point at a different
build of the executable. Reload the extension, select the browser inventory tab
in Destinations, and choose **Find nearby sync nodes**. Chrome asks for native
messaging permission on first use. Connect from the resulting list in the
inventory tab, or enter a WSS address manually in Sync.
