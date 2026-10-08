import type { ReactNode } from "react";
import Markdown, { type Components } from "react-markdown";
import { openUrl } from "@tauri-apps/plugin-opener";
import { format, isValid, parseISO } from "date-fns";
import { changelogSections } from "@/lib/changelog";
import { compareVersions } from "@/lib/semver";
import { cn } from "@/lib/utils";

const formatDate = (date: string) => {
  const parsed = parseISO(date);
  return isValid(parsed) ? format(parsed, "MMM d, yyyy") : "";
};

const subheading = ({ children }: { children?: ReactNode }) => (
  <p className="mt-2 mb-1 text-xs font-semibold leading-5">{children}</p>
);

// Compact elements for the 350 px tab; the global base styles (big margins, text-xl headings) are for pages.
const markdownComponents: Components = {
  h1: subheading,
  h2: subheading,
  h3: subheading,
  h4: subheading,
  h5: subheading,
  h6: subheading,
  p: ({ children }) => <p className="my-1.5 leading-5">{children}</p>,
  ul: ({ children }) => <ul className="my-1.5 ml-4 list-disc">{children}</ul>,
  ol: ({ children }) => <ol className="my-1.5 ml-4 list-decimal">{children}</ol>,
  // Margin on the item itself: it overrides the base `ul > li` margin, which `space-y` leaves on the last item.
  li: ({ children }) => <li className="mt-1.5 first:mt-0 leading-5 [&>p]:my-0">{children}</li>,
  strong: ({ children }) => <strong className="font-semibold">{children}</strong>,
  code: ({ children }) => <code className="rounded-sm bg-slate-100 px-1 py-px font-mono text-[11px]">{children}</code>,
  pre: ({ children }) => (
    <pre className="my-1.5 overflow-x-auto rounded-md bg-slate-100 p-2 [&>code]:bg-transparent [&>code]:p-0">
      {children}
    </pre>
  ),
  blockquote: ({ children }) => <blockquote className="my-1.5 border-l-2 border-slate-200 pl-3">{children}</blockquote>,
  hr: () => <hr className="my-2 border-slate-200" />,
  // Open links in the system browser instead of navigating the webview.
  a: ({ href, children }) => (
    <a
      href={href}
      className="underline underline-offset-2 decoration-slate-300 hover:decoration-current"
      onClick={(e) => {
        e.preventDefault();
        if (href) {
          openUrl(href).catch((error) => console.error("Failed to open link:", error));
        }
      }}
    >
      {children}
    </a>
  ),
};

/** The "What's new" tab: the bundled `CHANGELOG.md`, with versions newer than `sinceVersion` marked new. */
export const WhatsNew = ({ sinceVersion }: { sinceVersion: string | null }) => {
  return (
    <div className="flex flex-col p-2">
      <h4>What's new</h4>
      {changelogSections.length === 0 ?
        <span className="muted mt-3">No release notes in this build.</span>
      : <div className="flex flex-col gap-5 mt-3 mb-2">
          {changelogSections.map((section) => {
            const isNew = sinceVersion !== null && compareVersions(section.version, sinceVersion) > 0;
            const isMuted = sinceVersion !== null && !isNew;
            return (
              <section key={section.version} className={cn("text-xs", isMuted ? "text-slate-500" : "text-slate-700")}>
                <div className="flex items-center gap-2">
                  <h3
                    className={cn(
                      "text-sm font-semibold tracking-normal",
                      isMuted ? "text-slate-500" : "text-slate-900",
                    )}
                  >
                    {section.version}
                  </h3>
                  {section.date && <span className="text-xs text-slate-400">{formatDate(section.date)}</span>}
                  {isNew && (
                    <span className="rounded-sm bg-slate-900 px-1.5 text-[10px] font-medium leading-4 text-white">
                      New
                    </span>
                  )}
                </div>
                <Markdown components={markdownComponents} skipHtml disallowedElements={["img"]}>
                  {section.body}
                </Markdown>
              </section>
            );
          })}
        </div>
      }
    </div>
  );
};
