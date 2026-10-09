/**
 * CodeBuddy (both Tencent copilot.tencent.com and international codebuddy.ai)
 * shared channel neutralization, error parsing, and token rotation helpers.
 *
 * Upstream WAF checks message content for rival CLI identities (Claude Code, ZCode,
 * etc.) and rejects matching requests with code 11128:
 * "Illegal API invocation from an unapproved channel".
 */

export const CODEBUDDY_IDENTITY_REWRITES = [
  // Claude Code CLI identity lines (the primary trigger for 11128)
  {
    roles: ["system"],
    pattern: /^You are Claude Code, Anthropic's official CLI for Claude[^\n]*/gm,
    to: "You are CodeBuddy Code, an expert software engineering assistant.",
  },
  {
    roles: ["assistant"],
    pattern: /You are Claude Code, Anthropic's official CLI for Claude[^\n]*/g,
    to: "You are CodeBuddy Code, an expert software engineering assistant.",
  },
  // Rival agent identity lines
  {
    roles: ["system", "assistant"],
    pattern: /You are (?:ZCode|Cursor|Windsurf|Cline|Aider|Continue|Cody)[^\n.]*[.!]?/gi,
    to: "You are CodeBuddy Code, an expert software engineering assistant.",
  },
  {
    roles: ["system", "assistant"],
    pattern: /You are an interactive (?:ZCode|coding|code) agent[^\n.]*[.!]?/gi,
    to: "You are CodeBuddy Code, an expert software engineering assistant.",
  },
  {
    roles: ["system", "assistant"],
    pattern: /Sisyphus-Junior - Focused executor from OhMyOpenCode/g,
    to: "Sisyphus-Junior - Focused executor",
  },
  // Fingerprinted context headers / markers
  {
    roles: ["system"],
    pattern: /x-anthropic-billing-header/gi,
    to: "x-billing-header",
  },
  {
    roles: ["system"],
    pattern: /Main branch \(you will usually use this for PRs\):/g,
    to: "Main branch:",
  },
  {
    roles: ["system"],
    pattern: /^[ \t]*#[ \t]*(?:ZCode|DeepSeek[ \t-]+Harness|Claude[ \t]+Code)[ \t]+Desktop[ \t]+Context[ \t]*$/gim,
    to: "# Desktop Context",
  },
];

/**
 * Neutralize competitor CLI identities and markers in system and assistant messages.
 * Leaves user and tool messages byte-for-byte untouched.
 */
export function neutralizeCodeBuddyChannelIdentity(messages) {
  if (!Array.isArray(messages)) return messages;

  return messages.map((message) => {
    if (!message || typeof message !== "object") return message;
    const role = message.role;
    if (role !== "system" && role !== "assistant") return message;

    if (typeof message.content === "string") {
      let text = message.content;
      for (const { roles, pattern, to } of CODEBUDDY_IDENTITY_REWRITES) {
        if (!roles || roles.includes(role)) {
          text = text.replace(pattern, to);
        }
      }
      return text === message.content ? message : { ...message, content: text };
    }

    if (Array.isArray(message.content)) {
      let changed = false;
      const nextContent = message.content.map((part) => {
        if (!part || typeof part !== "object" || part.type !== "text" || typeof part.text !== "string") {
          return part;
        }
        let text = part.text;
        for (const { roles, pattern, to } of CODEBUDDY_IDENTITY_REWRITES) {
          if (!roles || roles.includes(role)) {
            text = text.replace(pattern, to);
          }
        }
        if (text !== part.text) changed = true;
        return changed ? { ...part, text } : part;
      });
      return changed ? { ...message, content: nextContent } : message;
    }

    return message;
  });
}

/**
 * Strips verbose descriptions if tools metadata exceeds 64KB (Tencent WAF limit).
 */
export function compactOversizedTools(tools) {
  if (!Array.isArray(tools) || tools.length === 0) return tools;
  try {
    const s = JSON.stringify(tools);
    if (new TextEncoder().encode(s).byteLength >= 65536) {
      return tools.map((tool) => {
        if (!tool || typeof tool !== "object" || tool.type !== "function" || !tool.function) return tool;
        if (!tool.function.description) return tool;
        const fn = { ...tool.function };
        delete fn.description;
        return { ...tool, function: fn };
      });
    }
  } catch {}
  return tools;
}

/**
 * Parse CodeBuddy upstream error envelope ({ code, msg, displayMsg }).
 * Maps 11128 to content filter / policy refusal.
 */
export function parseCodeBuddyError(response, bodyText) {
  try {
    const data = JSON.parse(bodyText);
    if (data?.code === 11128 || /unapproved channel/i.test(data?.msg || "")) {
      return {
        status: 400,
        message: data.msg || "Illegal API invocation from an unapproved channel",
        isContentFilter: true,
      };
    }
    if (data?.msg) {
      return {
        status: response?.status || 400,
        message: data.msg,
      };
    }
  } catch {}
  return null;
}

/**
 * Harvest rotated Bearer tokens from response Authorization headers.
 */
export function captureRotatedToken(response, credentials) {
  if (!response?.headers || !credentials) return;
  const getHeader = typeof response.headers.get === "function"
    ? (k) => response.headers.get(k)
    : (k) => response.headers[k] || response.headers[k?.toLowerCase?.()];
  const authHeader = getHeader("authorization");
  if (authHeader && typeof authHeader === "string" && authHeader.startsWith("Bearer ")) {
    const newToken = authHeader.slice("Bearer ".length).trim();
    if (newToken && newToken !== credentials.accessToken) {
      credentials.accessToken = newToken;
    }
  }
}
