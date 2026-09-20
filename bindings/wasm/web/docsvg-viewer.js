const TEMPLATE = `
  <style>
    :host {
      --docsvg-ink: #172033;
      --docsvg-muted: #58657a;
      --docsvg-border: #d7dfeb;
      --docsvg-accent: #2458c6;
      display: block;
      min-width: 0;
      color: var(--docsvg-ink);
      font: 14px/1.45 system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
    }
    * { box-sizing: border-box; }
    button, input { font: inherit; }
    button {
      min-height: 36px;
      border: 1px solid var(--docsvg-border);
      border-radius: 7px;
      background: #fff;
      color: var(--docsvg-ink);
      padding: 6px 10px;
      cursor: pointer;
    }
    button:hover { background: #f2f6fc; }
    button:focus-visible, input:focus-visible, .page-list:focus-visible {
      outline: 3px solid #82aaff;
      outline-offset: 2px;
    }
    button:disabled { cursor: default; opacity: .48; }
    .shell { overflow: hidden; border: 1px solid var(--docsvg-border); border-radius: 12px; background: #f4f7fb; }
    :host(:fullscreen) { width: 100vw; height: 100vh; background: #f4f7fb; }
    :host(:fullscreen) .shell { display: flex; width: 100%; height: 100%; flex-direction: column; border: 0; border-radius: 0; }
    :host(:fullscreen) .workspace { flex: 1 1 auto; height: auto; min-height: 0; }
    .toolbar {
      display: flex;
      flex-wrap: wrap;
      min-width: 0;
      align-items: center;
      gap: 7px;
      min-height: 56px;
      padding: 9px 12px;
      border-bottom: 1px solid var(--docsvg-border);
      background: #fff;
    }
    .toolbar .grow { flex: 1 1 0; min-width: 0; }
    .toolbar .group { display: inline-flex; min-width: 0; max-width: 100%; flex-wrap: wrap; align-items: center; gap: 5px; }
    .toolbar output { min-width: 62px; text-align: center; color: var(--docsvg-muted); font-variant-numeric: tabular-nums; }
    .toolbar input[type="number"] { width: 64px; min-height: 36px; border: 1px solid var(--docsvg-border); border-radius: 7px; padding: 4px 6px; text-align: center; }
    .toolbar input[type="search"] { width: 126px; min-height: 36px; border: 1px solid var(--docsvg-border); border-radius: 7px; padding: 4px 8px; }
    .toolbar input[type="file"] { display: none; }
    .toolbar .primary { border-color: var(--docsvg-accent); background: var(--docsvg-accent); color: #fff; }
    .toolbar .primary:hover { background: #1948aa; }
    .status {
      min-height: 34px;
      padding: 7px 14px;
      color: var(--docsvg-muted);
      background: #fff;
    }
    .status[data-kind="error"] { color: #9f2020; background: #fff2f0; }
    .status[data-kind="warning"] { color: #77520a; background: #fff8e4; }
    .document-warnings { padding: 7px 14px 9px; border-top: 1px solid #f0dfa7; color: #77520a; background: #fff8e4; font-size: 12px; }
    .document-warnings summary { cursor: pointer; }
    .document-warnings ul { max-width: 80ch; padding-inline-start: 22px; }
    .workspace { display: grid; grid-template-columns: 138px minmax(0, 1fr); height: min(78vh, 1100px); min-height: 420px; }
    .workspace.no-thumbnails { grid-template-columns: minmax(0, 1fr); }
    .thumbnails { overflow: auto; border-right: 1px solid var(--docsvg-border); padding: 10px; background: #eaf0f8; }
    .thumbnails[hidden] { display: none; }
    .thumb {
      display: block;
      width: 100%;
      min-height: 32px;
      margin: 0 0 7px;
      padding: 6px 8px;
      text-align: left;
      color: var(--docsvg-muted);
      font-variant-numeric: tabular-nums;
    }
    .thumb[aria-current="page"] { border-color: var(--docsvg-accent); color: var(--docsvg-accent); background: #edf3ff; }
    .page-list {
      overflow: auto;
      min-width: 0;
      padding: 18px clamp(10px, 2vw, 28px) 40px;
      overscroll-behavior: contain;
      scroll-behavior: smooth;
      background-color: #dfe6f0;
      background-image: radial-gradient(#c1ccda .7px, transparent .7px);
      background-size: 12px 12px;
    }
    .page-card {
      width: max-content;
      max-width: 100%;
      margin: 0 auto 22px;
      padding: 9px;
      border: 1px solid #ccd5e1;
      border-radius: 5px;
      background: #fff;
      box-shadow: 0 4px 14px rgb(28 43 67 / 14%);
      content-visibility: auto;
      contain-intrinsic-size: 900px 1100px;
      scroll-margin-block: 18px;
    }
    .page-stage { position: relative; max-width: 100%; margin: 0 auto; }
    .page-card img {
      position: absolute;
      top: 50%;
      left: 50%;
      display: block;
      max-width: none;
      transform: translate(-50%, -50%) rotate(var(--page-rotation, 0deg));
      background: white;
    }
    .text-layer {
      position: absolute;
      top: 50%;
      left: 50%;
      z-index: 1;
      transform: translate(-50%, -50%) rotate(var(--page-rotation, 0deg));
      transform-origin: center;
      pointer-events: none;
    }
    .text-span {
      position: absolute;
      display: block;
      overflow: hidden;
      color: transparent;
      line-height: 1.15;
      white-space: pre;
      transform-origin: 0 0;
      pointer-events: auto;
      user-select: text;
    }
    .text-span::selection { color: transparent; background: rgb(54 117 255 / 40%); }
    .text-span[data-search-match] { background: rgb(255 221 83 / 42%); }
    .text-span[data-search-current] { background: rgb(255 139 36 / 52%); }
    .page-label { margin: 7px 0 0; color: var(--docsvg-muted); font-size: 12px; text-align: center; }
    .page-warnings { max-width: 100%; margin-top: 8px; color: #77520a; font-size: 12px; }
    .page-warnings summary { cursor: pointer; }
    .page-warnings ul { max-width: 60ch; padding-inline-start: 22px; }
    .empty {
      display: grid;
      min-height: 100%;
      place-content: center;
      gap: 9px;
      padding: 32px;
      color: var(--docsvg-muted);
      text-align: center;
    }
    .empty strong { color: var(--docsvg-ink); font-size: 18px; }
    .empty[hidden] { display: none; }
    .source-name { max-width: min(28vw, 280px); overflow: hidden; color: var(--docsvg-muted); text-overflow: ellipsis; white-space: nowrap; }
    @media (max-width: 700px) {
      .workspace { grid-template-columns: 84px minmax(0, 1fr); height: 74vh; min-height: 360px; }
      .thumbnails { padding: 7px; }
      .toolbar { gap: 5px; padding: 8px; }
      .toolbar .group { max-width: 100%; }
      .toolbar button { min-height: 34px; padding-inline: 8px; }
      .toolbar input[type="search"] { width: 112px; }
      .source-name { max-width: 36vw; }
    }
    @media (prefers-color-scheme: dark) {
      :host {
        --docsvg-ink: #e5ebf5;
        --docsvg-muted: #a7b4c9;
        --docsvg-border: #38445a;
        --docsvg-accent: #82aaff;
      }
      .shell, button, .toolbar, .status { background: #1c2533; }
      button { color: var(--docsvg-ink); }
      button:hover { background: #29364a; }
      .thumbnails { background: #202c3d; }
      .thumb[aria-current="page"] { color: #a9c3ff; background: #263754; }
      .page-list { background-color: #111923; background-image: radial-gradient(#29364a .7px, transparent .7px); }
      .page-card { border-color: #303b4b; background: white; }
      .status[data-kind="error"] { color: #ffb4ab; background: #381b1c; }
      .status[data-kind="warning"], .document-warnings { color: #f6d98e; background: #342b19; }
    }
  </style>
  <section class="shell" aria-label="Document viewer">
    <div class="toolbar" role="toolbar" aria-label="Document controls">
      <button class="primary" type="button" data-action="pick">Open document</button>
      <input type="file" data-role="file" accept=".pdf,.docx,.docm,.dotx,.dotm,.xlsx,.xlsm,.xltx,.xltm,.xlam,.pptx,.pptm,.potx,.potm,.ppsx,.ppam,.sldx,.sldm">
      <span class="source-name" data-role="source"></span>
      <span class="grow"></span>
      <span class="group" aria-label="Page navigation">
        <button type="button" data-action="previous" aria-label="Previous page">‹</button>
        <input type="number" data-role="page" min="1" value="0" aria-label="Page number">
        <output data-role="page-count">/ 0</output>
        <button type="button" data-action="next" aria-label="Next page">›</button>
      </span>
      <span class="group" role="search" aria-label="Search document text">
        <input type="search" data-role="search" placeholder="Find text" aria-label="Find text in document" disabled>
        <button type="button" data-action="search-prev" aria-label="Previous search result" disabled>↑</button>
        <button type="button" data-action="search-next" aria-label="Next search result" disabled>↓</button>
        <output data-role="search-count" aria-live="polite">0/0</output>
      </span>
      <span class="group" aria-label="Zoom controls">
        <button type="button" data-action="zoom-out" aria-label="Zoom out">−</button>
        <output data-role="zoom">100%</output>
        <button type="button" data-action="zoom-in" aria-label="Zoom in">+</button>
        <button type="button" data-action="fit-width">Fit width</button>
        <button type="button" data-action="fit-page">Fit page</button>
        <button type="button" data-action="rotate" aria-label="Rotate page 90 degrees">Rotate</button>
        <button type="button" data-action="fullscreen">Full screen</button>
        </span>
      <button type="button" data-action="download" disabled>Save page SVG</button>
    </div>
    <div class="status" data-role="status" role="status" aria-live="polite">Choose a PDF, DOCX, XLSX, or PPTX file to preview.</div>
    <details class="document-warnings" data-role="warnings" hidden><summary></summary><ul></ul></details>
    <div class="workspace no-thumbnails" data-role="workspace">
      <nav class="thumbnails" data-role="thumbnails" aria-label="Document pages" hidden></nav>
      <main class="page-list" data-role="pages" tabindex="0" aria-label="Document pages">
        <div class="empty" data-role="empty"><strong>Private, in-browser preview</strong><span>Your document stays on this device. Conversion runs in a Web Worker.</span></div>
      </main>
    </div>
  </section>
`

const DEFAULT_MAX_INPUT_BYTES = 64 * 1024 * 1024
const MAX_SEARCH_RESULTS = 20_000

export class DocSvgViewer extends HTMLElement {
  #root
  #worker
  #requestId = 0
  #pages = []
  #cards = []
  #searchMatches = []
  #searchIndex = -1
  #searchTruncated = false
  #warningCount = 0
  #scale = 1
  #fitMode = 'custom'
  #currentPage = 0
  #rotation = 0
  #scrollUpdatePending = false
  #pageObserver
  #lazyObserver
  #resizeObserver
  #dragDepth = 0
  #onFullscreenChange = () => {
    const button = this.#root.querySelector('[data-action="fullscreen"]')
    if (button) button.textContent = document.fullscreenElement === this ? 'Exit full screen' : 'Full screen'
  }

  constructor() {
    super()
    this.#root = this.attachShadow({ mode: 'open' })
    this.#root.innerHTML = TEMPLATE
    this.#wireControls()
    this.#pageObserver = new IntersectionObserver((entries) => this.#observePages(entries), {
      root: this.#root.querySelector('[data-role="pages"]'),
      threshold: [0.15, 0.5, 0.85],
    })
    this.#lazyObserver = new IntersectionObserver((entries) => this.#observeImages(entries), {
      root: this.#root.querySelector('[data-role="pages"]'),
      rootMargin: '1000px 0px',
      threshold: 0,
    })
    this.#resizeObserver = new ResizeObserver(() => {
      if (this.#fitMode === 'width') this.#fitWidth()
      else if (this.#fitMode === 'page') this.#fitPage()
      else this.#sizePageImages()
    })
    const pageList = this.#root.querySelector('[data-role="pages"]')
    pageList.addEventListener('scroll', () => this.#scheduleVisiblePageUpdate(), { passive: true })
    this.#resizeObserver.observe(pageList)
  }

  set options(value) {
    this._options = value && typeof value === 'object' ? { ...value } : {}
  }

  get options() {
    return { ...(this._options ?? {}) }
  }

  connectedCallback() {
    document.addEventListener('fullscreenchange', this.#onFullscreenChange)
    this.#root.querySelector('[data-role="thumbnails"]').hidden = !this.hasAttribute('thumbnails')
    this.#root.querySelector('[data-role="workspace"]').classList.toggle('no-thumbnails', !this.hasAttribute('thumbnails'))
    this.#setupDragDrop()
  }

  disconnectedCallback() {
    document.removeEventListener('fullscreenchange', this.#onFullscreenChange)
    this.#stopWorker()
    this.#releasePages()
    this.#pageObserver.disconnect()
    this.#lazyObserver.disconnect()
    this.#resizeObserver.disconnect()
  }

  #wireControls() {
    this.#root.addEventListener('click', (event) => {
      const button = event.target.closest('[data-action]')
      if (!button) return
      switch (button.dataset.action) {
        case 'pick':
          this.#root.querySelector('[data-role="file"]').click()
          break
        case 'previous': this.#goToPage(this.#currentPage - 1); break
        case 'next': this.#goToPage(this.#currentPage + 1); break
        case 'zoom-out': this.#setScale(this.#scale / 1.2); break
        case 'zoom-in': this.#setScale(this.#scale * 1.2); break
        case 'fit-width': this.#fitWidth(); break
        case 'fit-page': this.#fitPage(); break
        case 'rotate': this.#rotate(); break
        case 'fullscreen': this.#toggleFullscreen(); break
        case 'search-prev': this.#moveSearch(-1); break
        case 'search-next': this.#moveSearch(1); break
        case 'download': this.#downloadPage(); break
        case 'thumb': this.#goToPage(Number(button.dataset.page)); break
      }
    })
    const fileInput = this.#root.querySelector('[data-role="file"]')
    fileInput.addEventListener('change', () => {
      const file = fileInput.files?.[0]
      fileInput.value = ''
      if (file) this.openFile(file)
    })
    const pageInput = this.#root.querySelector('[data-role="page"]')
    pageInput.addEventListener('change', () => this.#goToPage(Number(pageInput.value)))
    this.#root.querySelector('[data-role="search"]').addEventListener('input', () => this.#updateSearch())
    this.#root.querySelector('[data-role="pages"]').addEventListener('keydown', (event) => {
      if (event.key === 'ArrowDown' || event.key === 'PageDown') {
        event.preventDefault()
        this.#goToPage(this.#currentPage + 1)
      } else if (event.key === 'ArrowUp' || event.key === 'PageUp') {
        event.preventDefault()
        this.#goToPage(this.#currentPage - 1)
      } else if (event.key === '+' || event.key === '=') {
        this.#setScale(this.#scale * 1.2)
      } else if (event.key === '-') {
        this.#setScale(this.#scale / 1.2)
      } else if (event.key.toLowerCase() === 'r') {
        this.#rotate()
      } else if (event.key.toLowerCase() === 'f') {
        this.#fitWidth()
      }
    })
  }

  #setupDragDrop() {
    this.addEventListener('dragenter', (event) => {
      if (!event.dataTransfer?.types.includes('Files')) return
      event.preventDefault()
      this.#dragDepth += 1
      this.#setStatus('Drop a document to preview it.', 'warning')
    })
    this.addEventListener('dragover', (event) => {
      if (!event.dataTransfer?.types.includes('Files')) return
      event.preventDefault()
      event.dataTransfer.dropEffect = 'copy'
    })
    this.addEventListener('dragleave', (event) => {
      if (!event.dataTransfer?.types.includes('Files')) return
      event.preventDefault()
      this.#dragDepth = Math.max(0, this.#dragDepth - 1)
      if (this.#dragDepth === 0) this.#setStatus(this.#statusForPages())
    })
    this.addEventListener('drop', (event) => {
      if (!event.dataTransfer?.files?.length) return
      event.preventDefault()
      this.#dragDepth = 0
      this.openFile(event.dataTransfer.files[0])
    })
  }

  async openFile(file) {
    this.#stopWorker()
    const requestId = ++this.#requestId
    this.#releasePages()
    this.#currentPage = 0
    this.#rotation = 0
    this.#fitMode = 'custom'
    this.#scale = 1
    this.#root.querySelector('[data-role="zoom"]').textContent = '100%'
    this.#root.querySelector('[data-role="source"]').textContent = file.name
    const warningDetails = this.#root.querySelector('[data-role="warnings"]')
    this.#warningCount = 0
    warningDetails.hidden = true
    warningDetails.querySelector('ul').replaceChildren()
    const maxInputBytes = this.options.maxInputBytes ?? DEFAULT_MAX_INPUT_BYTES
    if (file.size > maxInputBytes) {
      this.#setStatus(`File is ${(file.size / 1024 / 1024).toFixed(1)} MiB; the configured limit is ${(maxInputBytes / 1024 / 1024).toFixed(0)} MiB.`, 'error')
      return
    }
    this.#root.querySelector('[data-role="empty"]').hidden = true
    this.#root.querySelector('[data-role="status"]').dataset.kind = ''
    this.#setStatus(`Reading ${file.name}…`)
    try {
      const bytes = await file.arrayBuffer()
      if (requestId !== this.#requestId) return
      const worker = new Worker(new URL('./docsvg-worker.js?v=browser-viewer-4', import.meta.url), { type: 'module', name: 'document-svg-preview' })
      this.#worker = worker
      worker.addEventListener('message', (event) => this.#onWorkerMessage(requestId, event.data))
      worker.addEventListener('error', (event) => {
        if (requestId !== this.#requestId) return
        this.#setStatus(`Preview worker failed: ${event.message || 'unknown error'}`, 'error')
        this.#stopWorker()
      })
      worker.postMessage({ type: 'convert', requestId, fileName: file.name, bytes, options: this.options }, [bytes])
      this.#setStatus(`Converting ${file.name}…`)
    } catch (error) {
      if (requestId === this.#requestId) this.#setStatus(`Cannot open file: ${this.#errorMessage(error)}`, 'error')
    }
  }

  #onWorkerMessage(requestId, message) {
    if (requestId !== this.#requestId || message?.requestId !== requestId) return
    if (message.type === 'ready') {
      this.#setStatus(`Rendering ${this.#root.querySelector('[data-role="source"]').textContent}…`)
    } else if (message.type === 'page') {
      this.#appendPage(message.page)
      this.#setStatus(`Rendered page ${message.page.number}…`)
    } else if (message.type === 'done') {
      const report = message.report
      this.#warningCount = report.warnings.length
      this.#setStatus(`${report.pageCount} page${report.pageCount === 1 ? '' : 's'} ready${report.warnings.length ? ` · ${report.warnings.length} conversion warning${report.warnings.length === 1 ? '' : 's'}` : ''}.`, report.warnings.length ? 'warning' : '')
      if (report.warnings.length) {
        const details = this.#root.querySelector('[data-role="warnings"]')
        details.querySelector('summary').textContent = `${report.warnings.length} document warning${report.warnings.length === 1 ? '' : 's'}`
        const list = details.querySelector('ul')
        for (const warning of report.warnings) {
          const item = document.createElement('li')
          item.textContent = warning
          list.append(item)
        }
        details.hidden = false
      }
      this.#stopWorker()
      if (this.#pages.length) this.#goToPage(1, false)
    } else if (message.type === 'error') {
      if (message.stack) console.error('Document preview failed', message.stack)
      this.#setStatus(`Cannot preview this document: ${message.message}`, 'error')
      this.#stopWorker()
      this.#root.querySelector('[data-role="empty"]').hidden = this.#pages.length > 0
    }
  }

  #appendPage(page) {
    const pages = this.#root.querySelector('[data-role="pages"]')
    const article = document.createElement('article')
    article.className = 'page-card'
    article.dataset.page = String(page.number)
    article.setAttribute('aria-label', `Page ${page.number}`)
    const image = document.createElement('img')
    image.alt = `Page ${page.number}`
    image.decoding = 'async'
    image.draggable = false
    const stage = document.createElement('div')
    stage.className = 'page-stage'
    const textLayer = document.createElement('div')
    textLayer.className = 'text-layer'
    for (const [spanIndex, textSpan] of (page.textSpans ?? []).entries()) {
      const span = document.createElement('span')
      span.className = 'text-span'
      span.dataset.spanIndex = String(spanIndex)
      span.textContent = textSpan.text
      span.setAttribute('aria-label', textSpan.text)
      textLayer.append(span)
    }
    stage.append(image, textLayer)
    const label = document.createElement('div')
    label.className = 'page-label'
    label.textContent = `Page ${page.number}`
    article.append(stage, label)
    if (page.warnings?.length) {
      const details = document.createElement('details')
      details.className = 'page-warnings'
      const summary = document.createElement('summary')
      summary.textContent = `${page.warnings.length} page warning${page.warnings.length === 1 ? '' : 's'}`
      const list = document.createElement('ul')
      for (const warning of page.warnings) {
        const item = document.createElement('li')
        item.textContent = warning
        list.append(item)
      }
      details.append(summary, list)
      article.append(details)
    }
    pages.append(article)
    const index = this.#pages.push({ ...page, url: null }) - 1
    this.#cards.push({ article, image, textLayer, index })
    this.#pageObserver.observe(article)
    this.#lazyObserver.observe(article)
    this.#appendThumbnail(page.number, index)
    this.#sizeImage(this.#cards[index])
    this.#root.querySelector('[data-role="empty"]').hidden = true
    this.#root.querySelector('[data-role="page-count"]').textContent = `/ ${this.#pages.length}`
    this.#root.querySelector('[data-role="page"]').max = String(this.#pages.length)
    this.#root.querySelector('[data-action="download"]').disabled = false
    this.#root.querySelector('[data-role="search"]').disabled = !this.#pages.some((item) => item.textSpans?.length)
    if (this.#root.querySelector('[data-role="search"]').value) this.#updateSearch()
  }

  #appendThumbnail(number, index) {
    if (!this.hasAttribute('thumbnails')) return
    const button = document.createElement('button')
    button.className = 'thumb'
    button.type = 'button'
    button.dataset.action = 'thumb'
    button.dataset.page = String(number)
    button.textContent = `Page ${number}`
    button.setAttribute('aria-label', `Go to page ${number}`)
    button.dataset.index = String(index)
    this.#root.querySelector('[data-role="thumbnails"]').append(button)
  }

  #observeImages(entries) {
    for (const entry of entries) {
      const card = this.#cards.find((item) => item.article === entry.target)
      if (!card) continue
      if (entry.isIntersecting) this.#loadImage(card)
      else this.#releaseImage(card)
    }
  }

  #loadImage(card) {
    const page = this.#pages[card.index]
    if (page.url) return
    page.url = URL.createObjectURL(new Blob([page.svg], { type: 'image/svg+xml;charset=utf-8' }))
    card.image.src = page.url
  }

  #releaseImage(card) {
    const page = this.#pages[card.index]
    if (!page?.url) return
    card.image.removeAttribute('src')
    URL.revokeObjectURL(page.url)
    page.url = null
  }

  #observePages(entries) {
    if (entries.some((entry) => entry.isIntersecting)) this.#scheduleVisiblePageUpdate()
  }

  #scheduleVisiblePageUpdate() {
    if (this.#scrollUpdatePending) return
    this.#scrollUpdatePending = true
    requestAnimationFrame(() => {
      this.#scrollUpdatePending = false
      const pageList = this.#root.querySelector('[data-role="pages"]')
      const viewport = pageList.getBoundingClientRect()
      const center = viewport.top + pageList.clientHeight / 2
      let closest = null
      let closestDistance = Infinity
      for (const card of this.#cards) {
        const bounds = card.article.getBoundingClientRect()
        if (bounds.bottom <= viewport.top || bounds.top >= viewport.bottom) continue
        const distance = Math.abs((bounds.top + bounds.bottom) / 2 - center)
        if (distance < closestDistance) {
          closest = card
          closestDistance = distance
        }
      }
      if (closest) this.#setCurrentPage(Number(closest.article.dataset.page))
    })
  }

  #setCurrentPage(number) {
    this.#currentPage = number
    const pageInput = this.#root.querySelector('[data-role="page"]')
    pageInput.value = String(number)
    pageInput.max = String(this.#pages.length)
    this.#root.querySelectorAll('.thumb').forEach((button) => {
      if (Number(button.dataset.page) === number) button.setAttribute('aria-current', 'page')
      else button.removeAttribute('aria-current')
    })
    if (!this.#worker) {
      const warningText = this.#warningCount
        ? ` · ${this.#warningCount} conversion warning${this.#warningCount === 1 ? '' : 's'}`
        : ''
      this.#setStatus(`Page ${number} of ${this.#pages.length}${warningText}.`, this.#warningCount ? 'warning' : '')
    }
  }

  #goToPage(number, updateStatus = true) {
    const bounded = Math.max(1, Math.min(this.#pages.length, Number.isFinite(number) ? Math.trunc(number) : 1))
    if (!this.#pages.length) return
    this.#cards[bounded - 1]?.article.scrollIntoView({ behavior: 'smooth', block: 'start' })
    this.#setCurrentPage(bounded)
    if (updateStatus) this.#setStatus(`Page ${bounded} of ${this.#pages.length}.`)
  }

  #setScale(value, mode = 'custom') {
    this.#fitMode = mode
    this.#scale = Math.max(0.05, Math.min(4, value))
    this.#root.querySelector('[data-role="zoom"]').textContent = `${Math.round(this.#scale * 100)}%`
    this.#sizePageImages()
    this.#restoreCurrentView()
  }

  #fitWidth() {
    const page = this.#pages[this.#currentPage - 1]
    if (!page) return
    const list = this.#root.querySelector('[data-role="pages"]')
    const available = Math.max(160, list.clientWidth - 56)
    const width = this.#rotation % 180 ? page.heightPoints : page.widthPoints
    this.#setScale(available / (width * 4 / 3), 'width')
  }

  #fitPage() {
    const page = this.#pages[this.#currentPage - 1]
    if (!page) return
    const list = this.#root.querySelector('[data-role="pages"]')
    const availableWidth = Math.max(160, list.clientWidth - 56)
    const availableHeight = Math.max(160, list.clientHeight - 56)
    const rotated = this.#rotation % 180 !== 0
    const width = rotated ? page.heightPoints : page.widthPoints
    const height = rotated ? page.widthPoints : page.heightPoints
    this.#setScale(Math.min(
      availableWidth / (width * 4 / 3),
      availableHeight / (height * 4 / 3),
    ), 'page')
  }

  #rotate() {
    this.#rotation = (this.#rotation + 90) % 360
    if (this.#fitMode === 'width') this.#fitWidth()
    else if (this.#fitMode === 'page') this.#fitPage()
    else {
      this.#sizePageImages()
      this.#restoreCurrentView()
    }
  }

  #restoreCurrentView() {
    const match = this.#searchIndex >= 0 ? this.#searchMatches[this.#searchIndex] : null
    const target = match
      ? this.#cards[match.pageIndex]?.textLayer.querySelector(`[data-span-index="${match.spanIndex}"]`)
      : this.#cards[this.#currentPage - 1]?.article
    target?.scrollIntoView({ behavior: 'auto', block: match ? 'center' : 'start' })
    this.#scheduleVisiblePageUpdate()
  }

  async #toggleFullscreen() {
    try {
      if (document.fullscreenElement === this) await document.exitFullscreen()
      else await this.requestFullscreen()
      this.#root.querySelector('[data-action="fullscreen"]').textContent =
        document.fullscreenElement === this ? 'Exit full screen' : 'Full screen'
    } catch (error) {
      this.#setStatus(`Full screen is unavailable: ${this.#errorMessage(error)}`, 'warning')
    }
  }

  #sizePageImages() {
    for (const card of this.#cards) this.#sizeImage(card)
    this.#scheduleVisiblePageUpdate()
  }

  #sizeImage(card) {
    const page = this.#pages[card.index]
    if (!page) return
    const sourceWidth = page.widthPoints * 4 / 3 * this.#scale
    const sourceHeight = page.heightPoints * 4 / 3 * this.#scale
    const rotated = this.#rotation % 180 !== 0
    const stage = card.article.querySelector('.page-stage')
    const textLayer = card.textLayer
    stage.style.width = `${rotated ? sourceHeight : sourceWidth}px`
    stage.style.height = `${rotated ? sourceWidth : sourceHeight}px`
    stage.style.setProperty('--page-rotation', `${this.#rotation}deg`)
    card.image.style.width = `${sourceWidth}px`
    card.image.style.height = `${sourceHeight}px`
    textLayer.style.width = `${sourceWidth}px`
    textLayer.style.height = `${sourceHeight}px`
    textLayer.style.setProperty('--page-rotation', `${this.#rotation}deg`)
    const scale = 4 / 3 * this.#scale
    for (const [spanIndex, span] of (page.textSpans ?? []).entries()) {
      const element = textLayer.querySelector(`[data-span-index="${spanIndex}"]`)
      if (!element) continue
      const [a, b, c, d, e, f] = span.transform
      element.style.left = `${span.x * scale}px`
      element.style.top = `${span.y * scale}px`
      element.style.width = `${Math.max(1, span.width * scale)}px`
      element.style.height = `${Math.max(1, span.height * scale)}px`
      element.style.fontSize = `${Math.max(1, span.fontSize * scale)}px`
      element.style.transform = `matrix(${a}, ${b}, ${c}, ${d}, ${e * scale}, ${f * scale})`
    }
  }

  #downloadPage() {
    const page = this.#pages[this.#currentPage - 1]
    if (!page) return
    const url = URL.createObjectURL(new Blob([page.svg], { type: 'image/svg+xml;charset=utf-8' }))
    const anchor = document.createElement('a')
    anchor.href = url
    anchor.download = `page-${String(page.number).padStart(4, '0')}.svg`
    anchor.click()
    setTimeout(() => URL.revokeObjectURL(url), 1000)
  }

  #updateSearch() {
    const query = this.#root.querySelector('[data-role="search"]').value.trim().toLocaleLowerCase()
    this.#searchMatches = []
    this.#searchIndex = -1
    this.#searchTruncated = false
    searchPages: for (let pageIndex = 0; pageIndex < this.#pages.length; pageIndex += 1) {
      const spans = this.#pages[pageIndex].textSpans ?? []
      for (let spanIndex = 0; spanIndex < spans.length; spanIndex += 1) {
        const text = spans[spanIndex].text.toLocaleLowerCase()
        if (!query) continue
        let cursor = 0
        while ((cursor = text.indexOf(query, cursor)) !== -1) {
          this.#searchMatches.push({ pageIndex, spanIndex })
          cursor += Math.max(1, query.length)
          if (this.#searchMatches.length >= MAX_SEARCH_RESULTS) {
            this.#searchTruncated = true
            break searchPages
          }
        }
      }
    }
    this.#applySearchMarkers()
    const hasMatches = this.#searchMatches.length > 0
    this.#root.querySelector('[data-action="search-prev"]').disabled = !hasMatches
    this.#root.querySelector('[data-action="search-next"]').disabled = !hasMatches
    this.#root.querySelector('[data-role="search-count"]').textContent = hasMatches ? `1/${this.#searchMatches.length}` : '0/0'
    if (hasMatches && this.#searchTruncated) {
      this.#root.querySelector('[data-role="search-count"]').textContent += '+'
    }
    if (hasMatches) {
      this.#searchIndex = 0
      this.#moveSearch(0)
    } else if (query) {
      this.#setStatus('No text matches.')
    }
  }

  #applySearchMarkers() {
    for (const card of this.#cards) {
      card.textLayer.querySelectorAll('.text-span').forEach((span) => {
        span.removeAttribute('data-search-match')
        span.removeAttribute('data-search-current')
      })
    }
    for (const match of this.#searchMatches) {
      const span = this.#cards[match.pageIndex]?.textLayer.querySelector(`[data-span-index="${match.spanIndex}"]`)
      if (span) span.setAttribute('data-search-match', '')
    }
    if (this.#searchIndex >= 0) {
      const current = this.#searchMatches[this.#searchIndex]
      const span = this.#cards[current.pageIndex]?.textLayer.querySelector(`[data-span-index="${current.spanIndex}"]`)
      if (span) span.setAttribute('data-search-current', '')
    }
  }

  #moveSearch(direction) {
    if (!this.#searchMatches.length) return
    this.#searchIndex = (this.#searchIndex + direction + this.#searchMatches.length) % this.#searchMatches.length
    this.#applySearchMarkers()
    const match = this.#searchMatches[this.#searchIndex]
    this.#goToPage(match.pageIndex + 1)
    const span = this.#cards[match.pageIndex]?.textLayer.querySelector(`[data-span-index="${match.spanIndex}"]`)
    span?.scrollIntoView({ behavior: 'smooth', block: 'center' })
    this.#root.querySelector('[data-role="search-count"]').textContent =
      `${this.#searchIndex + 1}/${this.#searchMatches.length}${this.#searchTruncated ? '+' : ''}`
  }

  #stopWorker() {
    this.#worker?.terminate()
    this.#worker = undefined
  }

  #releasePages() {
    for (const card of this.#cards) {
      this.#pageObserver.unobserve(card.article)
      this.#lazyObserver.unobserve(card.article)
      this.#releaseImage(card)
    }
    this.#cards = []
    this.#pages = []
    this.#warningCount = 0
    this.#root.querySelector('[data-role="pages"]').replaceChildren(this.#root.querySelector('[data-role="empty"]'))
    this.#root.querySelector('[data-role="empty"]').hidden = false
    this.#root.querySelector('[data-role="thumbnails"]').replaceChildren()
    this.#root.querySelector('[data-role="page-count"]').textContent = '/ 0'
    this.#root.querySelector('[data-role="page"]').value = '0'
    this.#root.querySelector('[data-role="page"]').max = '0'
    this.#root.querySelector('[data-action="download"]').disabled = true
    const search = this.#root.querySelector('[data-role="search"]')
    search.disabled = true
    search.value = ''
    this.#root.querySelector('[data-role="search-count"]').textContent = '0/0'
    this.#searchMatches = []
    this.#searchIndex = -1
    this.#searchTruncated = false
  }

  #setStatus(text, kind = '') {
    const status = this.#root.querySelector('[data-role="status"]')
    status.textContent = text
    status.dataset.kind = kind
  }

  #statusForPages() {
    return this.#pages.length ? `${this.#pages.length} page${this.#pages.length === 1 ? '' : 's'} ready.` : 'Choose a PDF, DOCX, XLSX, or PPTX file to preview.'
  }

  #errorMessage(error) {
    return error instanceof Error ? error.message : String(error)
  }
}

if (!customElements.get('docsvg-viewer')) customElements.define('docsvg-viewer', DocSvgViewer)

export default DocSvgViewer
