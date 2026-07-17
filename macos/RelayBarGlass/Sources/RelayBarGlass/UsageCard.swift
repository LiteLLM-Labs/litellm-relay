import SwiftUI

/// The single per-tool card of a light frosted-glass menu bar popover.
/// Everything reflects `model.selected` (one AI tool), scoped to the relay key —
/// hero spend and the daily spend chart lead, model mix is the headline feature,
/// insight chips are secondary.
struct UsageCard: View {
    @ObservedObject var model: AppModel

    // Chart geometry
    private let chartHeight: CGFloat = 48
    private let minBarHeight: CGFloat = 3

    private var selected: AgentInfo { model.selected }

    /// The provider accent that recolors the whole card.
    private var accent: Color { selected.accent }

    private var sortedDaily: [DailyPoint] {
        selected.dailySpend.sorted { $0.date < $1.date }
    }

    private var hasNoData: Bool {
        selected.spendMonth == 0 && selected.dailySpend.isEmpty
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            header

            if !model.didLoadOnce && model.lastError.isEmpty {
                loadingState
            } else if !model.didLoadOnce {
                errorState
            } else if !selected.routedViaRelay {
                notRoutedState
            } else if hasNoData {
                emptyState
            } else {
                hero
                chartSection
                modelMix
                insights
                footer
            }

            if model.keyBudget > 0 {
                budgetSection
            }
        }
        .padding(16)
        .frame(width: 364, alignment: .leading)
    }

    // MARK: - Header

    private var header: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 10) {
                AgentIcon(
                    resource: selected.iconResource,
                    fallback: selected.fallbackText,
                    size: 22
                )
                .padding(7)
                .background(
                    RoundedRectangle(cornerRadius: 10, style: .continuous)
                        .fill(accent.opacity(0.13))
                )

                Text(selected.name)
                    .font(GlassTheme.h1)
                    .fontWeight(.bold)
                    .foregroundColor(.white)
                    .lineLimit(1)
                    .truncationMode(.tail)
            }

            Text("relay key · updated just now")
                .font(GlassTheme.caption)
                .foregroundColor(Color.white.opacity(0.45))
                .lineLimit(1)
        }
    }

    // MARK: - Section label

    /// Small uppercase caption used above each section, matching the reference.
    private func sectionLabel(_ text: String) -> some View {
        Text(text.uppercased())
            .font(.system(size: 10, weight: .semibold))
            .tracking(0.9)
            .foregroundColor(Color.white.opacity(0.40))
    }

    // MARK: - Loading state

    /// Shown before the first spend fetch completes, so the card isn't stuck on
    /// seeded placeholder chrome that looks like a genuinely idle key.
    private var loadingState: some View {
        HStack(spacing: 8) {
            ProgressView()
                .controlSize(.small)
            Text("Loading usage…")
                .font(GlassTheme.body)
                .foregroundColor(GlassTheme.muted)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    // MARK: - Error state

    /// Shown when the first spend fetch fails (bad config, unreachable gateway,
    /// or — most commonly — a key that lacks the info/management routes). The
    /// old UI swallowed these and rendered an empty card indistinguishable from
    /// an idle key, which is exactly what made it "look empty for no reason".
    private var errorState: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Can't load usage")
                .font(GlassTheme.body)
                .foregroundColor(GlassTheme.muted)
                .fixedSize(horizontal: false, vertical: true)

            Text(model.lastError)
                .font(GlassTheme.caption)
                .foregroundColor(GlassTheme.textFaint)
                .fixedSize(horizontal: false, vertical: true)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    // MARK: - Empty state

    private var emptyState: some View {
        Text("No traffic on the relay key for \(selected.name) yet.")
            .font(GlassTheme.body)
            .foregroundColor(GlassTheme.muted)
            .fixedSize(horizontal: false, vertical: true)
            .frame(maxWidth: .infinity, alignment: .leading)
    }

    // MARK: - Not-routed state

    /// Shown for tools that authenticate with their own subscription (Codex App,
    /// Claude App, Cursor, Gemini, Copilot) rather than the relay key. These never
    /// produce relay spend, so we surface a clean explanation instead of a $0 hero
    /// stacked over empty charts.
    private var notRoutedState: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Not routed through Relay")
                .font(GlassTheme.body)
                .foregroundColor(GlassTheme.muted)
                .fixedSize(horizontal: false, vertical: true)

            Text("\(selected.name) uses its own login — route it through the relay key to see spend here.")
                .font(GlassTheme.caption)
                .foregroundColor(GlassTheme.textFaint)
                .fixedSize(horizontal: false, vertical: true)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    // MARK: - Hero spend

    private var hero: some View {
        VStack(alignment: .leading, spacing: 6) {
            sectionLabel("This month")

            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(String(format: "$%.2f", selected.spendMonth))
                    .font(.system(size: 34, weight: .bold).monospacedDigit())
                    .foregroundColor(accent)
                    .lineLimit(1)
                    .minimumScaleFactor(0.6)

                Text(AppModel.fmtTokens(selected.tokensMonth) + " tokens")
                    .font(GlassTheme.caption)
                    .foregroundColor(Color.white.opacity(0.45))
                    .monospacedDigit()
            }

            Text("Today "
                 + String(format: "$%.2f", selected.spendToday)
                 + " · "
                 + AppModel.fmtTokens(selected.tokensToday))
                .font(GlassTheme.caption)
                .foregroundColor(Color.white.opacity(0.45))
                .monospacedDigit()
                .lineLimit(1)
        }
    }

    // MARK: - Spend / day

    private var chartSection: some View {
        let points = sortedDaily
        let maxSpend = max(points.map(\.spend).max() ?? 0, 0.000_001)

        let barGradient = LinearGradient(
            colors: [accent.opacity(0.8), accent.opacity(0.45)],
            startPoint: .leading,
            endPoint: .trailing
        )

        return VStack(alignment: .leading, spacing: 6) {
            sectionLabel("Spend / day")

            if points.isEmpty {
                Text("no spend on the relay key yet")
                    .font(GlassTheme.caption)
                    .foregroundColor(GlassTheme.muted)
                    .frame(height: chartHeight, alignment: .center)
                    .frame(maxWidth: .infinity)
            } else {
                HStack(alignment: .bottom, spacing: 3) {
                    ForEach(points) { point in
                        RoundedRectangle(cornerRadius: 2, style: .continuous)
                            .fill(barGradient)
                            .frame(height: barHeight(for: point.spend, max: maxSpend))
                            .frame(maxWidth: .infinity)
                    }
                }
                .frame(height: chartHeight, alignment: .bottom)

                if points.count > 1, let first = points.first, let last = points.last {
                    HStack {
                        Text(first.date)
                            .font(GlassTheme.caption)
                            .foregroundColor(GlassTheme.textFaint)
                        Spacer(minLength: 4)
                        Text(last.date)
                            .font(GlassTheme.caption)
                            .foregroundColor(GlassTheme.textFaint)
                    }
                }
            }
        }
    }

    private func barHeight(for spend: Double, max maxSpend: Double) -> CGFloat {
        let ratio = CGFloat(spend / maxSpend)
        let scaled = ratio * chartHeight
        return Swift.max(minBarHeight, Swift.min(chartHeight, scaled))
    }

    // MARK: - Model mix (headline feature)

    private var modelMix: some View {
        let slices = selected.modelMix
        let maxSlice = max(slices.map(\.spend).max() ?? 0, 0.000_001)

        return VStack(alignment: .leading, spacing: 8) {
            sectionLabel("Model mix")

            if slices.isEmpty {
                Text("no model data yet")
                    .font(GlassTheme.caption)
                    .foregroundColor(GlassTheme.muted)
            } else {
                VStack(alignment: .leading, spacing: 10) {
                    ForEach(slices) { slice in
                        modelRow(slice, max: maxSlice)
                    }
                }
            }
        }
    }

    private func modelRow(_ slice: ModelSlice, max maxSlice: Double) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(slice.model)
                .font(GlassTheme.body)
                .foregroundColor(Color.white.opacity(0.70))
                .lineLimit(1)
                .truncationMode(.tail)

            HStack(spacing: 8) {
                GeometryReader { geo in
                    let ratio = CGFloat(slice.spend / maxSlice)
                    ZStack(alignment: .leading) {
                        Capsule(style: .continuous)
                            .fill(GlassTheme.track)
                        Capsule(style: .continuous)
                            .fill(accent)
                            .frame(width: max(2, geo.size.width * ratio))
                    }
                    .frame(maxHeight: .infinity, alignment: .center)
                }
                .frame(height: 7)

                Text(String(format: "$%.2f", slice.spend))
                    .font(GlassTheme.mono)
                    .foregroundColor(Color.white.opacity(0.45))
                    .monospacedDigit()
                    .lineLimit(1)
            }
        }
    }

    // MARK: - Insights (secondary)

    private var insights: some View {
        HStack(spacing: 8) {
            statChip(value: "\(Int(selected.cacheHitRate * 100))%", label: "Cache hit")
            statChip(value: "\(Int(selected.successRate * 100))%", label: "Success")
            statChip(value: String(format: "$%.2f", selected.costPerReq), label: "$/req")
        }
    }

    private func statChip(value: String, label: String) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(value)
                .font(GlassTheme.label)
                .monospacedDigit()
                .foregroundColor(GlassTheme.ink)
                .lineLimit(1)
                .minimumScaleFactor(0.7)

            Text(label)
                .font(GlassTheme.caption)
                .foregroundColor(GlassTheme.muted)
                .lineLimit(1)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.vertical, 8)
        .padding(.horizontal, 10)
        .glassCard()
    }

    // MARK: - Footer

    private var footer: some View {
        Text("Top model: " + (selected.topModel.isEmpty ? "—" : selected.topModel))
            .font(GlassTheme.caption)
            .foregroundColor(GlassTheme.muted)
            .lineLimit(1)
            .truncationMode(.tail)
            .frame(maxWidth: .infinity, alignment: .leading)
    }

    // MARK: - Relay-key budget (key-level, always at the bottom of the card)

    /// The fraction of the relay key's budget consumed, clamped to 1.
    private var budgetFraction: Double {
        guard model.keyBudget > 0 else { return 0 }
        return min(model.keySpend / model.keyBudget, 1)
    }

    private var budgetSection: some View {
        VStack(alignment: .leading, spacing: 6) {
            sectionLabel("Relay key budget")

            GeometryReader { geo in
                ZStack(alignment: .leading) {
                    Capsule(style: .continuous)
                        .fill(GlassTheme.track)
                    Capsule(style: .continuous)
                        .fill(accent)
                        .frame(width: max(2, geo.size.width * CGFloat(budgetFraction)))
                }
                .frame(maxHeight: .infinity, alignment: .center)
            }
            .frame(height: 8)

            HStack(alignment: .firstTextBaseline) {
                Text(budgetSpendText)
                    .font(GlassTheme.mono)
                    .foregroundColor(GlassTheme.ink)
                    .monospacedDigit()
                    .lineLimit(1)
                    .minimumScaleFactor(0.7)

                Spacer(minLength: 8)

                Text("\(Int(budgetFraction * 100))% used")
                    .font(GlassTheme.caption)
                    .foregroundColor(GlassTheme.muted)
                    .lineLimit(1)
            }

            Text("Resets " + formattedReset(model.budgetResetAt))
                .font(GlassTheme.caption)
                .foregroundColor(GlassTheme.textFaint)
                .lineLimit(1)
        }
    }

    /// "$12.34 / $1,000" — spend to 2 decimals, budget as a grouped whole number.
    private var budgetSpendText: String {
        let spend = String(format: "$%.2f", model.keySpend)
        return "\(spend) / \(budgetWhole(model.keyBudget))"
    }

    /// Formats a budget amount as a grouped whole-dollar string, e.g. "$1,000".
    private func budgetWhole(_ value: Double) -> String {
        let nf = NumberFormatter()
        nf.numberStyle = .decimal
        nf.maximumFractionDigits = 0
        nf.locale = Locale(identifier: "en_US")
        let formatted = nf.string(from: NSNumber(value: value)) ?? String(format: "%.0f", value)
        return "$\(formatted)"
    }

    /// Parses an ISO8601 reset timestamp and renders it like "Aug 1". Falls back
    /// to the first 10 characters of the raw string when parsing fails.
    private func formattedReset(_ iso: String) -> String {
        let withFraction = ISO8601DateFormatter()
        withFraction.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        let plain = ISO8601DateFormatter()
        plain.formatOptions = [.withInternetDateTime]

        guard let date = withFraction.date(from: iso) ?? plain.date(from: iso) else {
            return String(iso.prefix(10))
        }

        let out = DateFormatter()
        out.locale = Locale(identifier: "en_US_POSIX")
        out.dateFormat = "MMM d"
        return out.string(from: date)
    }
}
