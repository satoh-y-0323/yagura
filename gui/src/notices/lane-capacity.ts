/** How many cards the lane can show, worked out from its width alone. Pure. */

/** How many cards fit with nothing else on the lane, and how many fit beside the two counts. */
export interface LaneCapacity {
  all: number;
  withMore: number;
}

/**
 * The lane's sizes in px. The caller reads them from the stylesheet, so the
 * values have one source and are not copied here.
 */
export interface LaneMetrics {
  card: number;
  gap: number;
  padding: number;
  chip: number;
}

/**
 * Works out the lane's capacity at one width.
 * @param width the lane's width in px
 * @param metrics the lane's card, gap, inline padding and count widths in px
 * @returns at least one card each way, since a single card shrinks to the room left
 */
export function laneCapacity(width: number, metrics: LaneMetrics): LaneCapacity {
  const { card, gap, padding, chip } = metrics;
  // n cards take n·card + (n − 1)·gap, so adding one gap makes the room a whole number of slots.
  const fit = (room: number): number =>
    Math.max(1, Math.floor((room - 2 * padding + gap) / (card + gap)));
  // Both counts are set aside even when only one shows, so one formula covers every case.
  return { all: fit(width), withMore: fit(width - (2 * chip + 2 * gap)) };
}
