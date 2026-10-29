# site2PDF

Skrivbordsapp (Tauri + Rust) som hämtar en webbplats eller en lista med URL:er och sparar allt som **en sammanslagen PDF** eller **en Markdown-fil** (`.md`). Sidorna renderas i en riktig Chromium-webbläsare, så JavaScript-tunga sidor fungerar.

Byggd för att göra kunskapsstöd och dokumentationssajter läsbara offline, t.ex. [folkhalsaochsjukvard.rjl.se](https://folkhalsaochsjukvard.rjl.se/vardstod/fakta---allmant-kliniskt-kunskapsstod/).

## Ladda ned

Gå till [Releases](https://github.com/mrswedish/site2pdf/releases) och välj:

| Fil | Beskrivning |
|---|---|
| `site2pdf-portable.exe` | Portabel Windows-version, ingen installation behövs |
| `site2pdf-setup.exe` | Installationsprogram för Windows |

### Vilken webbläsare används?

Appen renderar sidorna med en Chromium-baserad webbläsare och letar i denna ordning:

1. Miljövariabeln `S2P_CHROME` (sökväg till en Chrome-/Chromium-/Edge-fil)
2. En mapp `chromium` **bredvid `.exe`-filen**, med samma struktur som nedladdningen, t.ex. `chromium\chrome-win64\chrome.exe` (för helt fristående/offline-användning)
3. Chrome for Testing som tidigare laddats ned till appens datamapp
4. **Redan installerad Google Chrome eller Microsoft Edge** (Edge finns på alla Windows 10/11-datorer)

Hittas ingen av dem laddas **Chrome for Testing** (~170 MB) ned första gången appen startas, från `googlechromelabs.github.io`. På datorer med Edge eller Chrome behövs alltså ingen nedladdning alls.

## Användning

Appen har två lägen:

- **Webbkrypning** – ange en start-URL. Appen hittar undersidor (se nedan) och slår ihop dem till en PDF.
- **URL-lista** – klistra in eller läs in en `.txt`-fil med en URL per rad. Exakt de sidorna hämtas, i den ordningen.

Välj var PDF:en ska sparas och klicka **Starta**.

### Format: PDF eller Markdown

Välj **Format** innan du väljer filnamn:

- **PDF** – sidorna som de ser ut, sammanslagna i en fil.
- **Markdown (.md)** – ett lättare textdokument. Rubriker, stycken, listor, tabeller, fetstil/kursiv och kodblock bevaras, **länkar behålls som `[text](url)`** (med fullständig adress) och **bilder som bildlänkar `![alt](url)`** (bilderna laddas inte ned; stäng av med *Ta med bildlänkar*). Varje sida får en rubrik och en rad `Källa: <url>`, och sidorna separeras med `---`. Dragspel fälls ut och kakbanners tas bort på samma sätt som för PDF.

Markdown-läget följer samma regler som PDF för vilka sidor som hämtas och för *Bara huvudinnehåll* (rekommenderas även här).

### Hur undersidor hittas

Appen följer länkar från startsidan ner till **Maxdjup** nivåer, men bara till sidor **under startsidans sökväg** (länkar till andra webbplatser följs aldrig). Är *Bara huvudinnehåll* på följs bara länkar i sidans huvudinnehåll, inte i menyer och sidopaneler.

Sidor som bara skiljer sig åt i region- eller spårningsparametrar (`globalregion`, `utm_*`, `accordionAnchor`, `pageId`, `blockId`, `gclid`, `fbclid`) räknas som samma sida.

Som standard används även **sitemap.xml** som komplement: appen läser `robots.txt`/`sitemap.xml` (även sitemap-index) och lägger sidorna under startsidans sökväg till som extra startpunkter. Sitemaps är ofta ofullständiga, så länkföljningen körs ändå ovanpå. Med *Bara sitemap* hämtas enbart sitemapens sidor och Maxdjup ignoreras.

### Avancerat

| Alternativ | Standard | Beskrivning |
|---|---|---|
| Blockera URL-mönster | – | Ett mönster per rad, `*` = wildcard, t.ex. `*/quiz/*`. Matchande sidor hoppas över. |
| Använd sitemap.xml | på | Lägger till sidor från sitemapen som startpunkter. Se ovan. |
| Bara sitemap | av | Följ inga länkar, hämta bara sitemapens sidor (Maxdjup ignoreras). |
| Bara huvudinnehåll | på | Döljer menyer, sidopaneler, filter, feedback och knappar, och följer bara länkar i huvudinnehållet. Fäller ut dragspel (`aria-expanded`, `<details>`) så att texten kommer med. |
| Ta med dokumentsidor | av | Hämtar även interna länkar i sidans huvudinnehåll som matchar mönstren nedan. Dokumentsidor följs inte vidare. |
| Dokumentlänk-mönster | `*/dokument/*` | Ett mönster per rad, `*` = wildcard, skiftlägesokänsligt. Gäller bara länkar på samma sajt. |
| Ta med bildlänkar | på | Bara för Markdown: tar med `![alt](url)`-referenser till bilder. |
| Förhandsgranska länkar | – | Skannar startsidan och ett urval sitemap-sidor, grupperar interna länkar per sökvägsdel (t.ex. `*/dokument/*`, 46 länkar) och låter dig kryssa i vilka som ska med. Valen skrivs in i mönsterfältet, som du även kan redigera för hand. Tar ca 30–60 sekunder. |
| Hantera kakbanners manuellt | av | Öppnar en synlig webbläsare där du själv godkänner kakor/loggar in. Behövs normalt inte. Kan webbläsaren inte öppnas körs automatisk kakhantering i stället. |

> **Tips:** `*/api/*`-länkar på rjl.se är PDF-filer, inte webbsidor, och är inte testade som dokumentsidor.

### Kakbanners

Samtyckesbanners godkänns automatiskt innan varje sida skrivs ut. Stödda system:

- **Cookie Information** (bl.a. rjl.se) via `CookieInformation.submitAllCategories()`
- **Cookiebot**, **OneTrust**, **Usercentrics**
- Generell reserv: knappar med text som *Godkänn / Acceptera / Accept all* m.fl., samt CSS som döljer kvarvarande bannrar

## Bygga själv

Krav: [Rust](https://rustup.rs) och Tauri CLI v2.

```bash
cargo install tauri-cli --version "^2" --locked
cd src-tauri
cargo tauri dev      # utvecklingsläge
cargo tauri build    # release-bygge
```

Windows-releaser byggs automatiskt av GitHub Actions ([`.github/workflows/build.yml`](.github/workflows/build.yml)) när en tagg `v*` pushas (tar ca 20 minuter).

```bash
git tag v0.4.0 && git push origin v0.4.0
```

Tester:

```bash
cd src-tauri
cargo test                                   # enhetstester
S2P_CHROME="/sökväg/till/chrome" cargo test -- --include-ignored   # inkl. nätverkstester mot rjl.se
```

## Projektstruktur

```
src/                    Frontend (HTML/JS/CSS, ingen build-steg)
src-tauri/src/
  crawler.rs            Krypning, kakhantering, huvudinnehåll, länkförhandsvisning
  sitemap.rs            Hämtar och tolkar sitemap.xml
  markdown.js           DOM → Markdown (körs i sidan, inkluderas i bygget)
  pdf.rs                Slår ihop PDF-sidor (lopdf)
  chromium_manager.rs   Laddar ned och hittar Chrome for Testing
  commands.rs           Tauri-kommandon som frontend anropar
```

## Kända begränsningar

- På macOS körs Chromium med `--single-process` för att undvika krascher. Hänger sig en vanlig installerad Chrome vid start, sätt miljövariabeln `S2P_NO_SINGLE=1`.
- Sidor som kräver inloggning kräver läget *Hantera kakbanners manuellt* (logga in i det öppnade fönstret).
- Förhandsgranskningen av länkar är ett urval; länkar som bara finns på ej skannade sidor kan missas.

## Licens

MIT
