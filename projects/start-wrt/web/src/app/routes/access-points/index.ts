import { Component, computed, inject } from '@angular/core'
import { TuiResponsiveDialogService } from '@taiga-ui/addon-mobile'
import { TuiButton, TuiCell, TuiTitle } from '@taiga-ui/core'
import { TUI_CONFIRM, TuiSkeleton } from '@taiga-ui/kit'
import { TuiHeader } from '@taiga-ui/layout'
import { provideFormService } from 'src/app/services/form.service'
import { i18nPipe } from 'src/app/i18n/i18n.pipe'
import { AccessPointCandidate } from 'src/app/services/api/api.service'
import { filter } from 'rxjs'

import { ADOPT_DIALOG, AdoptResult } from './dialog'
import { AccessPointsService } from './service'
import { AccessPointsTable } from './table'

@Component({
  template: `
    <header tuiHeader>
      <hgroup tuiTitle>
        <h2>{{ 'Access Points' | i18n }}</h2>
      </hgroup>
      @if (updatable()) {
        <aside tuiAccessories>
          <button
            tuiButton
            iconStart="@tui.download"
            (click)="service.update([])"
          >
            {{ 'Update all' | i18n }}
          </button>
        </aside>
      }
    </header>
    <table
      [tuiSkeleton]="!service.data()"
      [accessPoints]="service.data() || []"
    ></table>
    <header tuiHeader="h6">
      <hgroup tuiTitle>
        <h3>{{ 'Waiting to be adopted' | i18n }}</h3>
        <p tuiSubtitle>
          {{
            'Routers in the access point role that are cabled to a trunk port and waiting to be adopted.'
              | i18n
          }}
        </p>
      </hgroup>
    </header>
    @for (candidate of service.candidates(); track candidate.address) {
      <div tuiCell>
        <span tuiTitle>
          {{ candidate.address }}
          <span tuiSubtitle>
            {{ 'Version' | i18n }} {{ candidate.version }} ·
            {{
              (candidate.hasSticker
                ? 'Sticker Wi-Fi password'
                : 'Adoption code'
              ) | i18n
            }}
          </span>
        </span>
        <button tuiButton size="s" (click)="adopt(candidate)">
          {{ 'Adopt' | i18n }}
        </button>
      </div>
    } @empty {
      <p class="g-secondary">{{ 'No routers are waiting.' | i18n }}</p>
    }
  `,
  host: { class: 'g-page' },
  providers: [provideFormService(AccessPointsService)],
  imports: [
    TuiHeader,
    TuiTitle,
    TuiButton,
    TuiCell,
    TuiSkeleton,
    AccessPointsTable,
    i18nPipe,
  ],
})
export default class AccessPoints {
  private readonly dialogs = inject(TuiResponsiveDialogService)
  private readonly i18n = inject(i18nPipe)

  protected readonly service = inject(AccessPointsService)
  protected readonly updatable = computed(() =>
    (this.service.data() || []).some(item => this.service.updatable(item)),
  )

  adopt(candidate: AccessPointCandidate) {
    this.dialogs
      .open<AdoptResult>(ADOPT_DIALOG, {
        size: 's',
        data: { candidate, saved: this.service.saved() },
      })
      .subscribe(async result => {
        if ('remove' in result) {
          this.confirmRemove(candidate, result.remove)
        } else {
          const { name, secret } = result
          this.service.adopt({ address: candidate.address, name, secret })
        }
      })
  }

  private confirmRemove(candidate: AccessPointCandidate, names: string[]) {
    this.dialogs
      .open<boolean>(TUI_CONFIRM, {
        label: this.i18n.transform('Delete saved configurations'),
        data: {
          content: `${names.join(', ')}: ${this.i18n.transform(
            'their keys are revoked, and each must be adopted again.',
          )}`,
          yes: this.i18n.transform('Delete'),
          no: this.i18n.transform('Cancel'),
        },
      })
      .pipe(filter(Boolean))
      .subscribe(async () => {
        await this.service.removeSaved(names)
        this.adopt(candidate)
      })
  }
}
