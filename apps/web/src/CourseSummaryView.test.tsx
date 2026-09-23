// @vitest-environment jsdom
import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";
import { CourseSummaryView } from "./App";

afterEach(cleanup);

it("shows all source headings and keeps long chapter text collapsed behind a short source label", () => {
  const chapter = `Chapter fixture ${"unit ".repeat(16)}`;
  const { container } = render(<CourseSummaryView
    overview={[
      "承上启下", "Bridge fixture A",
      "主要内容", "Main fixture B",
      "内容讲解", "Explanation fixture C",
      "易错点", "Misconception fixture D",
    ].join("\n")}
    fallbackText=""
    points={[chapter]}
    terms={[{ term: "FixtureTerm", one_line: "Definition fixture" }]}
    teachingSections={[]}
  />);

  expect([...container.querySelectorAll(".course-summary-overview-sections h5")].map((heading) => heading.textContent)).toEqual([
    "承上启下", "主要内容", "内容讲解", "易错点",
  ]);
  expect(screen.getByText("Explanation fixture C")).toBeInTheDocument();
  expect(screen.getByText("FixtureTerm")).toBeInTheDocument();

  const details = container.querySelector(".course-summary-chapter")!;
  const summary = details.querySelector("summary")!;
  const fullText = details.querySelector("p")!;
  expect(details).not.toHaveAttribute("open");
  expect(summary.textContent?.length).toBeLessThan(chapter.length);
  expect(fullText.textContent).toBe(chapter);
  expect(fullText).not.toBeVisible();

  fireEvent.click(summary);
  expect(details).toHaveAttribute("open");
  expect(fullText).toBeVisible();
});

it("keeps an ordinary overview as supplied without adding the four section headings", () => {
  const { container } = render(<CourseSummaryView
    overview="Overview fixture only"
    fallbackText=""
    points={[]}
    terms={[]}
    teachingSections={[]}
  />);

  expect(screen.getByText("Overview fixture only")).toBeInTheDocument();
  expect(container.querySelector(".course-summary-overview-sections")).not.toBeInTheDocument();
});

it("uses a chapter's actual main point rather than its repeated section heading", () => {
  const point = ["承上启下", "", "主要内容", "1. Fault coverage depends on the fault model.",
    "内容讲解", "The selected model determines which faults count.", "易错点", "无"].join("\n");
  const { container } = render(<CourseSummaryView overview="课程概述" fallbackText=""
    points={[point]} terms={[]} teachingSections={[]} />);
  expect(container.querySelector(".course-summary-chapter > summary")?.textContent)
    .toBe("Fault coverage depends on the fault model.");
});

it("renders no overview or chapter content when generated fields are absent", () => {
  const { container } = render(<CourseSummaryView overview="" fallbackText="" points={[]} terms={[]} teachingSections={[]} />);
  expect(container).toBeEmptyDOMElement();
});
