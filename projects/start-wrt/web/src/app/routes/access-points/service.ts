import { inject, Injectable, signal } from '@angular/core'
import { FormService } from 'src/app/services/form.service'
import { i18nPipe } from 'src/app/i18n/i18n.pipe'
import { compareVersions } from 'src/app/utils/versions'
import {
  AccessPoint,
  AccessPointAdopt,
  AccessPointCandidate,
  ApiService,
  ProfileId,
} from 'src/app/services/api/api.service'

@Injectable()
export class AccessPointsService extends FormService<AccessPoint[]> {
  private readonly api = inject(ApiService)
  private readonly i18n = inject(i18nPipe)

  readonly candidates = signal<AccessPointCandidate[]>([])
  readonly profiles = signal<ProfileId[]>([])
  /** The Core's own firmware version, which an access point should match. */
  readonly coreVersion = signal('')

  async load(): Promise<AccessPoint[]> {
    const [list, candidates, profiles, info] = await Promise.all([
      this.api.satelliteList(),
      this.api.satelliteCandidates().catch(() => []),
      this.api.profilesList(),
      this.api.systemInfo().catch(() => null),
    ])

    this.candidates.set(candidates)
    this.profiles.set(profiles)
    if (info) this.coreVersion.set(info.version)

    return list
  }

  /** Running older firmware than the Core, or unable to read its configuration. */
  behind(item: AccessPoint): boolean {
    return item.needsUpdate || this.compare(item) < 0
  }

  newer(item: AccessPoint): boolean {
    return this.compare(item) > 0
  }

  /** Behind and not already queued or updating. */
  updatable(item: AccessPoint): boolean {
    return (
      this.behind(item) &&
      item.update?.state !== 'queued' &&
      item.update?.state !== 'updating'
    )
  }

  private compare(item: AccessPoint): number {
    const core = this.coreVersion()
    return item.version && core ? compareVersions(item.version, core) : 0
  }

  /** Names of access points out of contact: saved configurations a new
   *  router can take over (design §19). */
  saved(): string[] {
    return (this.data() || []).filter(s => !s.live).map(s => s.name)
  }

  async store(): Promise<void> {}

  adopt(params: AccessPointAdopt): Promise<boolean> {
    return this.actions.run(
      async () => {
        await this.api.satelliteAdopt(params)
        await this.refreshAndWait()
      },
      {
        loading: this.i18n.transform('Adopting...'),
        success: this.i18n.transform('Access point adopted'),
      },
    )
  }

  setPorts(
    name: string,
    ports: Record<string, number | null>,
  ): Promise<boolean> {
    return this.actions.run(
      async () => {
        await this.api.satelliteSetPorts({
          name,
          ports: Object.entries(ports).map(
            ([port, vlan]) => `${port}=${vlan === null ? 'off' : vlan}`,
          ),
        })
        await this.refreshAndWait()
      },
      { success: this.i18n.transform('Port assignments saved') },
    )
  }

  /** Updates `names`, or every access point behind the Core, one at a time. */
  update(names: string[]): Promise<boolean> {
    return this.actions.run(
      async () => {
        await this.api.satelliteUpdate({ names })
        await this.refreshAndWait()
      },
      { success: this.i18n.transform('Access point update started') },
    )
  }

  remove(name: string): Promise<boolean> {
    return this.actions.run(
      async () => {
        await this.api.satelliteUnpair({ name })
        await this.refreshAndWait()
      },
      { success: this.i18n.transform('Access point removed') },
    )
  }
}
