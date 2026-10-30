import { marked } from "marked";
import DOMPurify from "dompurify";

marked.setOptions({ gfm: true, breaks: true });

export function md(text: string): string {
  return DOMPurify.sanitize(marked.parse(text ?? "", { async: false }) as string);
}
