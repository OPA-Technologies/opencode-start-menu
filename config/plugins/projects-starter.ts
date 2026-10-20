import { appendFileSync, writeFileSync, existsSync } from "node:fs"
import { execSync } from "node:child_process"
import type { Plugin } from "@opencode-ai/plugin"

// Canal d'amorçage du menu opencode-start-menu.
//
// Le menu confie une mission à la session via deux variables
// d'environnement :
//   - OPENCODE_AUTO_RESUME : le prompt de résumé (relance d'un projet) ;
//   - OPENCODE_INSTRUCTION : une consigne ponctuelle (par ex. réécrire les
//     constantes du contrat RELEASE / STATUT / DATE_LAST_MAJ dans le format
//     du langage), honorée aussi sur un répertoire sans session préalable.
//
// Sans l'une ni l'autre, le plugin se contente d'afficher la liste des
// projets : c'est le lancement manuel d'opencode, hors menu.

const LOG_FILE = "/tmp/opencode-auto-resume.log"

if (!existsSync(LOG_FILE)) {
  writeFileSync(LOG_FILE, "")
}
const logLine = (msg: string) => {
  const line = `[projects-starter] ${new Date().toISOString()} ${msg}\n`
  try {
    appendFileSync(LOG_FILE, line)
  } catch {}
}

const withTimeout = <T>(p: Promise<T>, ms: number): Promise<T> =>
  Promise.race([
    p,
    new Promise<T>((_, reject) =>
      setTimeout(() => reject(new Error("timeout")), ms)
    ),
  ])

const plugin: Plugin = async ({ client }) => {
  const toast = async (
    message: string,
    variant: "info" | "success" | "warning" | "error" = "info"
  ) => {
    try {
      await client.tui.showToast({
        body: { title: "projects-starter", message, variant, duration: 6000 },
      })
    } catch {}
  }

  const resume = process.env.OPENCODE_AUTO_RESUME
  const instruction = process.env.OPENCODE_INSTRUCTION

  if (!resume && !instruction) {
    // Lancement manuel d'opencode : affiche la liste des projets existants
    try {
      const output = execSync(
        "bash /home/opa/bin/opencode-projects.sh list",
        { encoding: "utf8", timeout: 5000 }
      )
      process.stderr.write("\n" + output + "\n")
    } catch {
      // silencieux
    }
    return { event: async () => {} }
  }

  // Lancement via le menu (op) : résumé et/ou instruction ponctuelle.
  const targetDir = process.cwd()
  let targetSession: string | null = null
  let autoResume = false
  let phaseDone = false

  // Le résumé prime quand il est présent : la consigne ponctuelle le suit.
  const system = resume ?? instruction ?? ""
  const bulle = resume ? "Generating Resume and Todo..." : "Instruction du menu..."

  logLine(`amorcage actif (dir=${targetDir}, resume=${!!resume}, instruction=${!!instruction})`)
  toast(resume ? "Génération du résumé en cours..." : "Instruction transmise...")

  // Recherche la session du projet (la plus récente pour ce répertoire)
  const findSession = async (): Promise<string | null> => {
    try {
      const sessions: any[] = await withTimeout(
        client.session.list({ query: { directory: targetDir } }).then(
          (r: any) => (Array.isArray(r) ? r : r?.data ?? [])
        ),
        3000
      )
      const match = sessions
        .filter((s) => s?.directory === targetDir)
        .sort((a, b) => (b?.time?.created ?? 0) - (a?.time?.created ?? 0))
      return match[0]?.id ?? null
    } catch {
      return null
    }
  }

  // Injection + génération réelle (POST /session/{id}/message).
  // Ce endpoint crée le message utilisateur et DÉCLENCHE le loop
  // (contrairement à prompt_async qui n'enregistre que).
  const inject = async (sessionID: string) => {
    if (targetSession !== null) return
    targetSession = sessionID
    autoResume = true
    logLine(`found session ${sessionID} for ${targetDir}`)
    try {
      // Bascule le TUI sur cette session : quitte l'écran d'accueil
      // (welcome "OPENCODE Ask anything...") et affiche le chat.
      await client.tui
        .executeCommand({ body: { command: "/session " + sessionID } })
        .catch(() => {})
      await new Promise((r) => setTimeout(r, 700))
      await client.session.prompt({
        path: { id: sessionID },
        body: {
          system,
          parts: [{ type: "text", text: bulle }],
        },
      })
      if (!phaseDone) {
        phaseDone = true
        autoResume = false
        logLine(`amorcage genere dans ${sessionID}`)
        toast(`Amorçage généré (session ${sessionID})`, "success")
      }
    } catch (err) {
      logLine(`inject failed: ${err}`)
      toast(`Échec de l'amorçage : ${err}`, "error")
      autoResume = false
    }
  }

  // Sondage de secours : couvre le cas où opencode réutilise une session
  // existante (pas d'événement created/updated émis).
  ;(async () => {
    const deadline = Date.now() + 15000
    while (targetSession === null && Date.now() < deadline) {
      const id = await findSession()
      if (id) {
        inject(id)
        return
      }
      await new Promise((r) => setTimeout(r, 500))
    }
    if (targetSession === null) {
      logLine("no session found for target directory")
      toast("Aucune session trouvée pour ce projet", "warning")
    }
  })()

  return {
    event: async ({ event }) => {
      if (!event) return
      const ev = event as unknown as {
        type: string
        properties: {
          info?: { id?: string; directory?: string }
          sessionID?: string
          id?: string
        }
      }

      const sessionID =
        ev.properties.sessionID ?? ev.properties.info?.id ?? null
      const sessionDir = ev.properties.info?.directory ?? null

      // 1. Détection rapide (session fraîchement créée)
      if (
        targetSession === null &&
        (ev.type === "session.created" || ev.type === "session.updated") &&
        sessionDir === targetDir
      ) {
        if (!sessionID) return
        inject(sessionID)
        return
      }

      // 2. Autorise les permissions pendant la phase d'amorçage
      if (
        autoResume &&
        sessionID === targetSession &&
        (ev.type === "permission.asked" || ev.type === "permission.updated")
      ) {
        const permissionID = ev.properties.id
        if (permissionID) {
          try {
            await client.session
              .postSessionIdPermissionsPermissionId({
                path: { id: targetSession, permissionID },
                body: { response: "once" },
              })
              .catch(() => {})
          } catch {}
        }
        return
      }

      // 3. Fin de la phase (filet de sécurité si prompt a échoué)
      if (
        autoResume &&
        sessionID === targetSession &&
        ev.type === "session.idle"
      ) {
        autoResume = false
      }
    },
  }
}

export default plugin
