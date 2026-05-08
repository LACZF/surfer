import * as vscode from 'vscode'
import * as path from 'path'
import * as fs from 'fs'
import * as net from 'net'
import * as crypto from 'crypto'
import { ChildProcess, spawn } from 'child_process'

const SURVER_READY_TIMEOUT_MS = 15000
const SURVER_READY_POLL_MS = 250
// GitLab project ID for surfer-project/surfer.
// The extension downloads standalone surver binaries from CI artifacts using:
// https://gitlab.com/api/v4/projects/<project-id>/jobs/artifacts/main/raw/<artifact>?job=<job>
const SURVER_ARTIFACT_PROJECT_ID = '42073614'

let surverInstallPromise: Promise<string> | undefined

type SurverInstallMeta = {
  wellenVersion: string
  artifactPath: string
  job: string
  installedAtIso: string
}

class Wavefile implements vscode.CustomDocument {
  uri: vscode.Uri
  constructor(uri: vscode.Uri) {
    this.uri = uri
  }
  dispose(): void {}
}

function isSshRemoteSession(): boolean {
  return vscode.env.remoteName === 'ssh-remote'
}

// Read the extension's wellen version (the version that libsurfer was compiled with).
// This is stored in globalStorageUri when first available from the wasm module.
function getExtensionWellenVersionPath(context: vscode.ExtensionContext): string {
  return path.join(context.globalStorageUri.fsPath, 'extension-wellen-version.txt')
}

function readExtensionWellenVersion(context: vscode.ExtensionContext): string | undefined {
  const versionPath = getExtensionWellenVersionPath(context)
  if (!fs.existsSync(versionPath)) {
    return undefined
  }

  try {
    return fs.readFileSync(versionPath, 'utf8').trim()
  } catch {
    return undefined
  }
}

function writeExtensionWellenVersion(context: vscode.ExtensionContext, version: string): void {
  const versionPath = getExtensionWellenVersionPath(context)
  fs.mkdirSync(path.dirname(versionPath), { recursive: true })
  fs.writeFileSync(versionPath, version, 'utf8')
}

// Extract the wellen version from a surver binary by starting it briefly and querying /get_status.
// Returns the wellen version that the surver binary was compiled with.
async function getSurverWellenVersion(surverPath: string): Promise<string | undefined> {
  try {
    const port = await getFreePort()
    const token = makeSurverToken()
    const process = spawn(surverPath, ['--port', String(port), '--token', token], {
      stdio: ['ignore', 'pipe', 'pipe'],
      windowsHide: true,
    })

    try {
      // Wait briefly for surver to start
      await new Promise((resolve) => setTimeout(resolve, 500))

      const response = await fetch(`http://127.0.0.1:${port}/get_status`, { method: 'GET' })
      if (!response.ok) {
        return undefined
      }
      const status = (await response.json()) as { wellen_version?: string }
      return status.wellen_version
    } finally {
      process.kill()
    }
  } catch {
    return undefined
  }
}

function makeSurverToken(): string {
  // Surver requires a token with length >= 8.
  return crypto.randomBytes(16).toString('hex')
}

function pickWorkspaceSurverCommand(): string | undefined {
  const workspaceRoot = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath
  const candidates = [
    workspaceRoot ? path.join(workspaceRoot, 'target', 'release', 'surver') : undefined,
    workspaceRoot ? path.join(workspaceRoot, 'target', 'release', 'surver.exe') : undefined,
    workspaceRoot ? path.join(workspaceRoot, 'target', 'debug', 'surver') : undefined,
    workspaceRoot ? path.join(workspaceRoot, 'target', 'debug', 'surver.exe') : undefined,
  ].filter((c): c is string => Boolean(c))

  for (const candidate of candidates) {
    if (fs.existsSync(candidate)) {
      return candidate
    }
  }

  return undefined
}

function getSurverBinaryName(): string {
  return process.platform === 'win32' ? 'surver.exe' : 'surver'
}

function runCommandAndWait(command: string, args: string[], cwd?: string): Promise<number> {
  return new Promise((resolve) => {
    const child = spawn(command, args, {
      cwd,
      stdio: ['ignore', 'pipe', 'pipe'],
      windowsHide: true,
    })

    child.stdout.on('data', (buf: Buffer) => {
      console.log(`[${path.basename(command)}] ${buf.toString().trimEnd()}`)
    })
    child.stderr.on('data', (buf: Buffer) => {
      console.warn(`[${path.basename(command)}] ${buf.toString().trimEnd()}`)
    })

    child.on('close', (code) => {
      resolve(code ?? 1)
    })
    child.on('error', () => {
      resolve(1)
    })
  })
}

async function commandExists(command: string): Promise<boolean> {
  const code = await runCommandAndWait(command, ['--version'])
  return code === 0
}

function getWorkspaceRoot(): string | undefined {
  return vscode.workspace.workspaceFolders?.[0]?.uri.fsPath
}

async function tryBuildWorkspaceSurver(): Promise<string | undefined> {
  const workspaceRoot = getWorkspaceRoot()
  if (!workspaceRoot) {
    return undefined
  }

  const surverManifest = path.join(workspaceRoot, 'surver', 'Cargo.toml')
  if (!fs.existsSync(surverManifest)) {
    return undefined
  }

  const code = await runCommandAndWait('cargo', ['build', '--release', '--locked', '-p', 'surver'], workspaceRoot)
  if (code !== 0) {
    return undefined
  }

  return pickWorkspaceSurverCommand()
}

function getSurverStoragePath(context: vscode.ExtensionContext): string {
  return path.join(context.globalStorageUri.fsPath, 'surver', getSurverBinaryName())
}

function getSurverMetaPath(context: vscode.ExtensionContext): string {
  return path.join(context.globalStorageUri.fsPath, 'surver', 'install-meta.json')
}

function readSurverInstallMeta(context: vscode.ExtensionContext): SurverInstallMeta | undefined {
  const metaPath = getSurverMetaPath(context)
  if (!fs.existsSync(metaPath)) {
    return undefined
  }

  try {
    const raw = fs.readFileSync(metaPath, 'utf8')
    return JSON.parse(raw) as SurverInstallMeta
  } catch {
    return undefined
  }
}

function writeSurverInstallMeta(context: vscode.ExtensionContext, meta: SurverInstallMeta): void {
  const metaPath = getSurverMetaPath(context)
  fs.mkdirSync(path.dirname(metaPath), { recursive: true })
  fs.writeFileSync(metaPath, JSON.stringify(meta, null, 2), 'utf8')
}

function getSurverArtifactInfo(): { artifactPath: string; job: string } | undefined {
  const platform = process.platform
  const arch = process.arch

  // The artifactPath/job values here must match the files exposed by the
  // corresponding build jobs in .gitlab-ci.yml artifacts.paths.

  if (platform === 'linux' && arch === 'x64') {
    return { artifactPath: 'surver_linux', job: 'linux_build' }
  }
  if (platform === 'linux' && arch === 'arm64') {
    return { artifactPath: 'surver_linux_arm64', job: 'linux_arm64_build' }
  }
  if (platform === 'darwin' && arch === 'arm64') {
    return { artifactPath: 'surver_macos-aarch64', job: 'macos-aarch64_build' }
  }
  if (platform === 'win32' && arch === 'x64') {
    return { artifactPath: 'surver_win.exe', job: 'windows_build' }
  }

  return undefined
}

async function downloadToFile(url: string, destination: string): Promise<void> {
  const response = await fetch(url)
  if (!response.ok) {
    throw new Error(`Failed to download ${url}: HTTP ${response.status}`)
  }

  const data = Buffer.from(await response.arrayBuffer())
  fs.mkdirSync(path.dirname(destination), { recursive: true })
  fs.writeFileSync(destination, data)
}

async function installSurverFromArtifacts(context: vscode.ExtensionContext): Promise<string> {
  const artifact = getSurverArtifactInfo()
  if (!artifact) {
    throw new Error(`No auto-install artifact available for ${process.platform}/${process.arch}`)
  }

  const destination = getSurverStoragePath(context)

  // Example resolved URL:
  // https://gitlab.com/api/v4/projects/42073614/jobs/artifacts/main/raw/surver_linux?job=linux_build
  const url = `https://gitlab.com/api/v4/projects/${SURVER_ARTIFACT_PROJECT_ID}/jobs/artifacts/main/raw/${artifact.artifactPath}?job=${artifact.job}`
  await downloadToFile(url, destination)
  if (process.platform !== 'win32') {
    fs.chmodSync(destination, 0o755)
  }

  // Query the surver binary to get its actual wellen version.
  // This ensures the metadata stores the wellen version that the binary was compiled with.
  const wellenVersion = (await getSurverWellenVersion(destination)) ?? 'unknown'
  writeSurverInstallMeta(context, {
    wellenVersion,
    artifactPath: artifact.artifactPath,
    job: artifact.job,
    installedAtIso: new Date().toISOString(),
  })

  return destination
}

function isManagedSurverCurrent(context: vscode.ExtensionContext): boolean {
  const installedPath = getSurverStoragePath(context)
  if (!fs.existsSync(installedPath)) {
    return false
  }

  const expectedArtifact = getSurverArtifactInfo()
  if (!expectedArtifact) {
    return false
  }

  const meta = readSurverInstallMeta(context)
  if (!meta) {
    return false
  }

  // Compare the wellen version from the extension (compiled into libsurfer)
  // against the wellen version stored in metadata (what the cached surver was compiled with).
  const extensionWellenVersion = readExtensionWellenVersion(context) ?? 'unknown'
  return (
    meta.wellenVersion === extensionWellenVersion &&
    meta.artifactPath === expectedArtifact.artifactPath &&
    meta.job === expectedArtifact.job
  )
}

async function ensureManagedSurverUpToDate(context: vscode.ExtensionContext): Promise<string> {
  if (isManagedSurverCurrent(context)) {
    return getSurverStoragePath(context)
  }

  return installSurverFromArtifacts(context)
}

async function ensureSurverAvailable(context: vscode.ExtensionContext): Promise<string> {
  const workspaceCommand = pickWorkspaceSurverCommand()
  if (workspaceCommand) {
    return workspaceCommand
  }

  // Keep managed surver in sync with libsurfer's wellen version.
  // The wellen version is the critical compatibility point: libsurfer (wasm client) and surver
  // must be built with the same wellen version or protocol incompatibilities will occur.
  // We validate by comparing workspace Cargo.lock wellen version against the cached surver's
  // compiled wellen version (extracted at download time via /get_status endpoint).
  try {
    return await ensureManagedSurverUpToDate(context)
  } catch (err) {
    console.warn(`Surfer: managed surver update failed, falling back to local tools (${err})`)
  }

  const built = await tryBuildWorkspaceSurver()
  if (built) {
    return built
  }

  if (await commandExists('surver')) {
    return 'surver'
  }

  return ensureManagedSurverUpToDate(context)
}

async function getFreePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const server = net.createServer()
    server.unref()
    server.on('error', reject)
    server.listen(0, '127.0.0.1', () => {
      const address = server.address()
      if (!address || typeof address === 'string') {
        server.close(() => reject(new Error('Failed to allocate a TCP port for surver')))
        return
      }
      const port = address.port
      server.close((err) => {
        if (err) {
          reject(err)
          return
        }
        resolve(port)
      })
    })
  })
}

async function waitForSurverReady(baseUrl: string, timeoutMs: number): Promise<void> {
  const deadline = Date.now() + timeoutMs

  while (Date.now() < deadline) {
    try {
      const response = await fetch(`${baseUrl}/get_status`, { method: 'GET' })
      if (response.ok) {
        return
      }
    } catch {
      // keep polling until timeout
    }

    await new Promise((resolve) => setTimeout(resolve, SURVER_READY_POLL_MS))
  }

  throw new Error(`Timed out while waiting for surver at ${baseUrl}`)
}

async function startSurverForDocument(
  document: vscode.CustomDocument,
  context: vscode.ExtensionContext
): Promise<{
  process: ChildProcess
  externalUrl: string
}> {
  if (document.uri.scheme !== 'file') {
    throw new Error(`Remote surver mode only supports file:// URIs, got ${document.uri.scheme}`)
  }

  if (!surverInstallPromise) {
    surverInstallPromise = ensureSurverAvailable(context)
  }
  let command: string
  try {
    command = await surverInstallPromise
  } catch (err) {
    surverInstallPromise = undefined
    throw err
  }
  const port = await getFreePort()
  const token = makeSurverToken()
  const filePath = document.uri.fsPath

  const args = [
    '--port',
    String(port),
    '--bind-address',
    '127.0.0.1',
    '--token',
    token,
    filePath,
  ]

  const proc = spawn(command, args, {
    stdio: ['ignore', 'pipe', 'pipe'],
    windowsHide: true,
  })

  proc.stdout.on('data', (buf: Buffer) => {
    console.log(`[surver] ${buf.toString().trimEnd()}`)
  })
  proc.stderr.on('data', (buf: Buffer) => {
    console.warn(`[surver] ${buf.toString().trimEnd()}`)
  })

  const baseUrl = `http://127.0.0.1:${port}/${token}`

  try {
    await waitForSurverReady(baseUrl, SURVER_READY_TIMEOUT_MS)
  } catch (error) {
    proc.kill()
    throw error
  }

  const externalUri = await vscode.env.asExternalUri(vscode.Uri.parse(baseUrl, true))

  return {
    process: proc,
    externalUrl: externalUri.toString(),
  }
}

/** Maps an open-dialog `kind` to the Surfer `inject_message` JSON to fire once
 *  the user picks a file.  Returns `null` for unknown kinds. */
function openDialogMessage(kind: string, fileUri: vscode.Uri, webview: vscode.Webview): string | null {
  const url = webview.asWebviewUri(fileUri).toString()
  switch (kind) {
    case 'waveform_clear':
      return JSON.stringify({ LoadWaveformFileFromUrl: [url, 'Clear'] })
    case 'waveform_keep_available':
      return JSON.stringify({ LoadWaveformFileFromUrl: [url, 'KeepAvailable'] })
    case 'waveform_keep_all':
      return JSON.stringify({ LoadWaveformFileFromUrl: [url, 'KeepAll'] })
    case 'command_file':
      return JSON.stringify({ LoadCommandFileFromUrl: url })
    case 'state_file':
      return null
    default:
      return null
  }
}

function normalizeDialogFilters(
  filters: Array<{ name: string; extensions: string[] }> | undefined
): Record<string, string[]> {
  const filterRecord: Record<string, string[]> = {}

  for (const f of (filters ?? [])) {
    const normalized: string[] = []

    for (const extRaw of (f.extensions ?? [])) {
      const ext = (extRaw ?? '').trim().replace(/^\./, '')
      if (!ext) {
        continue
      }

      // VS Code dialog filters accept single extension segments (e.g. "ron").
      // Convert multi-part forms like "surf.ron" to their trailing segment.
      const tail = ext.split('.').filter(Boolean).pop()
      if (!tail) {
        continue
      }

      normalized.push(tail)
    }

    filterRecord[f.name] = Array.from(new Set(normalized))
  }

  return filterRecord
}

export class SurferWaveformViewerEditorProvider
  implements vscode.CustomReadonlyEditorProvider<Wavefile>
{
  public static register(context: vscode.ExtensionContext): vscode.Disposable {
    const provider = new SurferWaveformViewerEditorProvider(context)
    const providerRegistration = vscode.window.registerCustomEditorProvider(
      SurferWaveformViewerEditorProvider.viewType,
      provider,
      {
        webviewOptions: {
          retainContextWhenHidden: true,
        },
      }
    )

    return providerRegistration
  }
  private static readonly viewType = 'surfer.waveformViewer'

  constructor(private readonly context: vscode.ExtensionContext) {}
  async openCustomDocument(
    uri: vscode.Uri,
    _openContext: vscode.CustomDocumentOpenContext,
    _token: vscode.CancellationToken
  ): Promise<Wavefile> {
    return new Wavefile(uri)
  }
  async resolveCustomEditor(
    document: vscode.CustomDocument,
    webviewPanel: vscode.WebviewPanel,
    _token: vscode.CancellationToken
  ): Promise<void> {
    // Add the folder that the document lives in as a localResourceRoot
    const uriString = document.uri.toString()
    // TODO: this probably doesn't play well with windows
    const dirPath = uriString.substring(0, uriString.lastIndexOf('/'))

    webviewPanel.webview.options = {
      enableScripts: true,
      localResourceRoots: [
        vscode.Uri.joinPath(this.context.extensionUri, 'surfer'),
        vscode.Uri.parse(dirPath, true),
      ],
    }

    webviewPanel.webview.html = await this.getHtmlForWebview(webviewPanel.webview)

    const documentUri = webviewPanel.webview.asWebviewUri(document.uri).toString()
    let remoteUrlPromise: Promise<string> | undefined
    let surverProcess: ChildProcess | undefined

    const getLoadUrl = async (): Promise<string> => {
      if (!isSshRemoteSession()) {
        return documentUri
      }

      if (!remoteUrlPromise) {
        remoteUrlPromise = (async () => {
          const started = await startSurverForDocument(document, this.context)
          surverProcess = started.process
          return started.externalUrl
        })()
      }

      return remoteUrlPromise
    }

    webviewPanel.onDidDispose(() => {
      if (surverProcess && !surverProcess.killed) {
        surverProcess.kill()
      }
    })

    webviewPanel.webview.onDidReceiveMessage(async (message) => {
      switch (message.command) {
        case 'loaded': {
          console.log("Surfer got the loaded message from the web view")

          // Cache the extension's wellen version (obtained from the wasm module).
          // This is used to validate that downloaded surver binaries are compatible.
          if (message.wellenVersion && typeof message.wellenVersion === 'string') {
            writeExtensionWellenVersion(this.context, message.wellenVersion)
          }

          try {
            const loadUrl = await getLoadUrl()
            webviewPanel.webview.postMessage({command: "LoadUrl", url: loadUrl})
          } catch (e) {
            const msg = `Surfer: failed to start remote surver, falling back to direct file load (${e})`
            console.warn(msg)
            vscode.window.showWarningMessage(msg)
            webviewPanel.webview.postMessage({command: "LoadUrl", url: documentUri})
          }
          break;
        }

        case 'showOpenDialog': {
          const { kind, filters } = message
          const filterRecord = normalizeDialogFilters(filters)
          const uris = await vscode.window.showOpenDialog({
            canSelectMany: false,
            filters: filterRecord,
            title: message.title ?? 'Open file',
          })
          if (uris && uris.length > 0) {
            // Grant the webview access to the directory containing the chosen file.
            const chosenDir = vscode.Uri.joinPath(uris[0], '..')
            webviewPanel.webview.options = {
              ...webviewPanel.webview.options,
              localResourceRoots: [
                ...(webviewPanel.webview.options.localResourceRoots ?? []),
                chosenDir,
              ],
            }
            if (kind === 'state_file') {
              try {
                const bytes = await vscode.workspace.fs.readFile(uris[0])
                const msg = JSON.stringify({ LoadStateFromData: Array.from(bytes) })
                webviewPanel.webview.postMessage({ command: 'InjectMessage', message: msg })
              } catch (e) {
                vscode.window.showErrorMessage(`Surfer: failed to read state file: ${e}`)
              }
            } else {
              const msg = openDialogMessage(kind, uris[0], webviewPanel.webview)
              if (msg !== null) {
                webviewPanel.webview.postMessage({ command: 'InjectMessage', message: msg })
              } else {
                console.log(`Surfer: unknown open-dialog kind '${kind}'`)
              }
            }
          }
          break;
        }

        case 'saveState': {
          // Sent by the webview in response to 'RequestState'. Contains the
          // encoded state string and the target URI chosen in showSaveDialog.
          const targetUri = vscode.Uri.parse(message.targetUri, true)
          const encoded = new TextEncoder().encode(message.state)
          try {
            await vscode.workspace.fs.writeFile(targetUri, encoded)
          } catch (e) {
            vscode.window.showErrorMessage(`Surfer: failed to save state file: ${e}`)
          }
          break;
        }

        case 'vscodeSaveStateFromWasm': {
          // Sent by the Rust code via surfer_notify_host. Contains the encoded state.
          // Show the save dialog and write the file.
          const filterRecord: Record<string, string[]> = {
            'Surfer state files': ['ron'],
          }
          const defaultUri = vscode.Uri.file(
            require('path').join(require('os').homedir(), 'surfer_state.surf.ron')
          )
          const uri = await vscode.window.showSaveDialog({
            filters: filterRecord,
            title: 'Save Surfer state',
            defaultUri,
          })
          if (uri) {
            const encoded = new TextEncoder().encode(message.data)
            try {
              await vscode.workspace.fs.writeFile(uri, encoded)
            } catch (e) {
              vscode.window.showErrorMessage(`Surfer: failed to save state file: ${e}`)
            }
          }
          break;
        }

        case 'resolveProxyUrl': {
          const rawUrl = String(message.url ?? '')
          const requestId = String(message.requestId ?? '')

          let resolvedUrl = rawUrl
          try {
            const uri = vscode.Uri.parse(rawUrl, true)
            const proxied = await vscode.env.asExternalUri(uri)
            resolvedUrl = proxied.toString()
          } catch (e) {
            console.log(`Surfer: failed to resolve proxy URL '${rawUrl}', using original (${e})`)
          }

          webviewPanel.webview.postMessage({
            command: 'resolvedProxyUrl',
            requestId,
            url: resolvedUrl,
          })
          break;
        }

        default: {
          console.log(`Got unexpected command (${message.command}) from surfer web view`)
        }
      }
    })
  }

  // Get the static html used for the editor webviews.
  private async getHtmlForWebview(webview: vscode.Webview): Promise<string> {
    // Read index.html from disk
    const indexPath = vscode.Uri.joinPath(this.context.extensionUri, 'surfer', 'index.html')
    const contents = await vscode.workspace.fs.readFile(indexPath)

    // Replace local paths with paths derived from WebView URIs
    let html = new TextDecoder().decode(contents)

    const translate_url = (path: string[], in_html: string) => {
      const uriString = webview
        .asWebviewUri(vscode.Uri.joinPath(this.context.extensionUri, ...path))
        .toString()
      html = this.replaceAll(html, `["']${in_html}['"]`, `"${uriString}"`)
    }

    translate_url(["surfer", "manifest.json"], "/manifest.json");
    translate_url(["surfer", "surfer_bg.wasm"], "/surfer_bg.wasm");
    translate_url(["surfer", "surfer.js"], "/surfer.js");
    translate_url(["surfer", "sw.js"], "/sw.js");
    translate_url(["surfer", "integration.js"], "integration.js");

    // We need to wait until the new HTML has loaded before we can start injecting messages.
    // To detect this, we'll add a script at the every end of the html that sends a message
    // back to the host saying that the HTML is loaded.
    // A hacky but working way of doing this is to replace the </body> tag with a script
    // and a new </body> tag
    const vscodeApi = `acquireVsCodeApi()`
    const load_notifier = `
        (function() {
            const vscode = ${vscodeApi};
            // Store the vscode API globally for surfer_notify_host (from integration.js)
            window.__surfer_host_api = vscode;

            // Expose native VS Code file-dialog helpers to the Rust/WASM side.
            // These are called by the \`vscode\` feature build via wasm_bindgen externs.
            window.vscode_show_open_dialog = function(kind, filtersJson) {
                const filters = JSON.parse(filtersJson);
                vscode.postMessage({ command: 'showOpenDialog', kind, filters });
            };

            let __surfer_proxy_counter = 0;
            const __surfer_pending_proxy_requests = new Map();

            // Resolve remote URLs through the extension host (e.g. remote SSH port-forwarding).
            window.vscode_proxy_url = function(url) {
              return new Promise((resolve) => {
                const requestId = 'proxy_url_' + (__surfer_proxy_counter++);
                __surfer_pending_proxy_requests.set(requestId, resolve);
                vscode.postMessage({ command: 'resolveProxyUrl', requestId, url });
              });
            };

            // Listen for 'RequestState' from the host and reply with the current encoded state via get_state().
            window.addEventListener('message', async (event) => {
                if (event.data?.command === 'RequestState') {
                    const state = await get_state();
                    vscode.postMessage({ command: 'saveState', targetUri: event.data.targetUri, state });
                } else if (event.data?.command === 'InjectMessage') {
                    inject_message(event.data.message);
              } else if (event.data?.command === 'resolvedProxyUrl') {
                const resolve = __surfer_pending_proxy_requests.get(event.data.requestId);
                if (resolve) {
                  __surfer_pending_proxy_requests.delete(event.data.requestId);
                  resolve(event.data.url);
                }
                }
            });

            // Retrieve the extension's wellen version from the wasm module for cache validation.
            let wellenVersion = undefined;
            try {
              if (typeof get_wellen_version === 'function') {
                wellenVersion = get_wellen_version();
              }
            } catch (e) {
              // get_wellen_version may not be available yet, which is OK
            }

            vscode.postMessage({
                command: 'loaded',
                wellenVersion: wellenVersion,
            })
        }())`
    html = html.replaceAll("/*SURFER_SETUP_HOOKS*/", `${load_notifier}`)

    return html
  }

  private replaceAll(input: string, search: string, replacement: string): string {
    return input.replace(new RegExp(search, 'g'), replacement)
  }
}
