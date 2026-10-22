import { Component, inject, input } from '@angular/core'
import { FormsModule } from '@angular/forms'
import { RouterLink } from '@angular/router'
import { TuiResponsiveDialogService } from '@taiga-ui/addon-mobile'
import { TuiTable, TuiTableDirective } from '@taiga-ui/addon-table'
import {
  TuiAppearance,
  TuiDataList,
  TuiDropdown,
  TuiLink,
} from '@taiga-ui/core'
import {
  TUI_CONFIRM,
  TuiBadge,
  TuiButtonSelect,
  TuiChevron,
} from '@taiga-ui/kit'
import { firstValueFrom } from 'rxjs'

import { Placeholder } from 'src/app/components/placeholder'
import { i18nPipe } from 'src/app/i18n/i18n.pipe'
import { confirmPublishedPortDeletion } from 'src/app/services/published-port-deletion'

import { EthernetPortView, EthernetService } from './service'

/** Dropdown value of the managed-switch choice; a profile name is never empty. */
const TRUNK = ''

@Component({
  selector: '[ethernetTable]',
  template: `
    <thead>
      <tr>
        <th tuiTh [sorter]="'name' | tuiSorter">{{ 'Port' | i18n }}</th>
        <th tuiTh [sorter]="'profileName' | tuiSorter">
          {{ 'Security Profile' | i18n }}
        </th>
      </tr>
    </thead>
    <tbody>
      @for (item of ethernetTable() | tuiTableSort; track $index) {
        <tr>
          <td tuiTd>
            {{ item.name }}
            @if (item.wan) {
              <span tuiBadge size="s" appearance="primary">WAN</span>
            }
          </td>
          <td tuiTd>
            @if (item.wan) {
              -
            } @else {
              <button
                tuiLink
                tuiChevron
                tuiButtonSelect
                tuiDropdownAlign="start"
                [textContent]="item.profileName"
                [ngModel]="item.trunk ? TRUNK : item.profileName"
                (ngModelChange)="onProfileChange(item, $event)"
              >
                <tui-data-list *tuiDropdown>
                  @for (profile of service.profiles(); track profile.vlan_tag) {
                    <button tuiOption [value]="profile.fullname">
                      {{ profile.fullname }}
                    </button>
                  }
                  <hr />
                  <button tuiOption iconEnd="@tui.network" [value]="TRUNK">
                    {{ 'Managed switch' | i18n }}
                  </button>
                  <hr />
                  <a
                    tuiOption
                    tuiAppearance="action"
                    iconEnd="@tui.user-lock"
                    routerLink="/profiles"
                  >
                    {{ 'Manage Profiles' | i18n }}
                  </a>
                </tui-data-list>
              </button>
              @if (item.trunk) {
                <div class="g-secondary">
                  {{ 'VLAN IDs' | i18n }}:
                  @for (
                    profile of service.profiles();
                    track profile.vlan_tag;
                    let last = $last
                  ) {
                    {{ profile.fullname }} {{ profile.vlan_tag
                    }}{{ last ? '' : ',' }}
                  }
                </div>
              }
            }
          </td>
        </tr>
      } @empty {
        <tr>
          <td tuiTd colspan="2">
            <app-placeholder icon="@tui.inbox">
              {{ 'No ports detected' | i18n }}
            </app-placeholder>
          </td>
        </tr>
      }
    </tbody>
  `,
  styles: `
    :host {
      [tuiLink]::after {
        vertical-align: text-bottom;
      }

      [tuiBadge] {
        vertical-align: baseline;
        margin: 0 0.125rem;
      }
    }
  `,
  hostDirectives: [TuiTableDirective],
  host: { class: 'g-table' },
  imports: [
    TuiTable,
    TuiLink,
    TuiButtonSelect,
    FormsModule,
    TuiDataList,
    TuiDropdown,
    TuiChevron,
    TuiBadge,
    RouterLink,
    TuiAppearance,
    Placeholder,
    i18nPipe,
  ],
})
export class EthernetTable {
  protected readonly service = inject(EthernetService)
  private readonly dialogs = inject(TuiResponsiveDialogService)
  private readonly i18n = inject(i18nPipe)

  protected readonly TRUNK = TRUNK

  readonly ethernetTable = input<EthernetPortView[]>([])

  async onProfileChange(item: EthernetPortView, value: string) {
    const trunk = value === TRUNK
    if (trunk === item.trunk && (trunk || value === item.profileName)) return

    if (trunk && !(await this.confirmTrunk())) {
      this.service.refresh() // revert the dropdown
      return
    }

    item.trunk = trunk
    item.profile = trunk
      ? null
      : (this.service.profiles().find(p => p.fullname === value) ?? null)
    item.profileName = trunk
      ? this.i18n.transform('Managed switch')
      : (item.profile?.fullname ?? 'Admin')
    const items = [...this.ethernetTable()]

    // Reassigning this port moves its devices to a new subnet, breaking any
    // published ports forwarding to them. Surface them for confirmation first.
    let pending
    try {
      pending = await this.service.previewPorts(items)
    } catch {
      this.service.refresh() // nothing applied — restore the dropdown
      return
    }

    if (
      !(await confirmPublishedPortDeletion(this.dialogs, this.i18n, pending))
    ) {
      this.service.refresh() // revert the dropdown to the applied state
      return
    }

    this.service.save(items)
  }

  private confirmTrunk(): Promise<boolean> {
    return firstValueFrom(
      this.dialogs.open<boolean>(TUI_CONFIRM, {
        label: this.i18n.transform('Connect a managed switch'),
        data: {
          content: this.i18n.transform(
            'Every security profile is sent to this port as a tagged VLAN, and the switch decides which profile each of its own ports belongs to. Anyone who can log in to the switch can reach every profile, so protect its password and finish its VLAN setup before connecting it.',
          ),
          yes: this.i18n.transform('Trust this switch'),
          no: this.i18n.transform('Cancel'),
        },
      }),
    ).catch(() => false)
  }
}
