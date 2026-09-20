import initializeWasm, { convert_document } from './pkg/document_svg_wasm.js?v=browser-viewer-4'

let initialization

function errorMessage(error) {
  if (error instanceof Error) return error.message
  return String(error)
}

self.addEventListener('message', async (event) => {
  const message = event.data
  if (!message || message.type !== 'convert') return

  const { requestId, fileName, bytes, options = {} } = message
  try {
    initialization ??= initializeWasm({
      module_or_path: new URL('./pkg/document_svg_wasm_bg.wasm?v=browser-viewer-4', import.meta.url),
    })
    await initialization
    self.postMessage({ type: 'ready', requestId })
    const report = convert_document(
      fileName,
      new Uint8Array(bytes),
      options,
      (page) => self.postMessage({ type: 'page', requestId, page }),
    )
    self.postMessage({ type: 'done', requestId, report })
  } catch (error) {
    console.error('document-svg worker conversion failed', error)
    self.postMessage({
      type: 'error',
      requestId,
      message: errorMessage(error),
      stack: error instanceof Error ? error.stack : undefined,
    })
  }
})
