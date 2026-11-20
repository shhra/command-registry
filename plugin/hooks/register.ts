import type { Register } from 'claude-code'

const POLICY = `# Command registry (cr)
The user tracks the main runs of every task in \`cr\`, their command registry; the cr:cr-register skill has the full recipe.
- A task that runs more than one real command (builds, runs, tests, benchmarks, training, evals, data jobs, pipelines) gets a cr session: \`cr session new <task> -d '...'\`, host paths as \`cr session env <task> KEY=/abs/path\`, one \`CR_SESSION=<task> cr add <name> --by claude --cmd '...' -d '...'\` per step in run order, then \`cr run <name> --by claude\`.
- Check \`cr list\` / \`cr session list\` first and reuse an existing entry or session instead of adding a duplicate.
- A run longer than a few minutes: \`cr run <name> --by claude --detach\`, watched with \`cr ps\` and \`cr tail <name>\`, stopped with \`cr stop <name>\`.
- Never nohup, setsid, disown, a trailing \`&\`, pkill, or a wrapper script that hides the steps; never run_in_background except on \`cr run\` or \`cr tail\`.
- Quick inspection (ls, cat, grep, sed -n, git status/log/diff) runs directly and is not registered.`

const DETACHERS = /(^|[\s;&|(])(nohup|setsid|disown|pkill)(\s|$)/
const TRAILING_AMPERSAND = /(^|[^&>|])&[ \t]*(;|$)/m
const USES_CR = /(^|[\s;&|(])cr\s+(run|tail|ps|stop|add|session|list|show|log)\b/
const MAIN_RUN =
  /\bbazel(isk)?\s+(run|test)\b|\btorchrun\b|\bcargo\s+run\b|\bpython3?\b[^|;&\n]*\b(train|bench|eval|sweep|pipeline|recon|orion)\w*|(^|[\s;&])(\.\/|bash\s+|sh\s+)[\w./-]+\.sh\b/

export const register: Register = on => {
  const nudged = new Set<string>()

  on('prompt.compose', async ($, e, next) => {
    const composed = await next(e)
    return {
      sections: [...composed.sections, { id: 'cr:policy', text: POLICY, scope: 'session' }],
    }
  })

  on('tool.call', { tool: 'Bash' }, async ($, e, next) => {
    const command = e.command.split('<<')[0] ?? ''
    const viaCr = USES_CR.test(command)

    if (!viaCr && DETACHERS.test(command)) {
      return {
        deny: `${$.plugin.name}: no nohup/setsid/disown/pkill here. Register the command and start it with \`cr run <name> --by claude --detach\`; stop it with \`cr stop <name>\`.`,
      }
    }
    if (!viaCr && TRAILING_AMPERSAND.test(command)) {
      return {
        deny: `${$.plugin.name}: no backgrounding with \`&\`. Register the command and start it with \`cr run <name> --by claude --detach\`, then \`cr tail <name>\`.`,
      }
    }
    if (e.run_in_background === true && !viaCr) {
      return {
        deny: `${$.plugin.name}: background runs go through cr. Register the command (cr:cr-register) and run \`cr run <name> --by claude --detach\`, or background \`cr tail <name>\`.`,
      }
    }

    const ran = await next(e)
    const key = command.trim().slice(0, 80)
    if (viaCr || !MAIN_RUN.test(command) || nudged.has(key) || ran.deny !== undefined) {
      return ran
    }
    nudged.add(key)
    const reminder = `${$.plugin.name}: this looks like a main run that went straight to Bash. Register it in the task's cr session (cr:cr-register) and replay it with \`cr run <name> --by claude\` so the user can see and replay it.`
    return { ...ran, context: [...(ran.context ?? []), reminder] }
  }).catch(($, e, next) => next(e))
}
